//! Durable issuer statements and cut acknowledgments; `docs/runtime/mergeable-horizon.md`.
mod bootstrap;
use super::existence::ExistenceBaseAttachment;
use super::*;
use rocky_credentials::{
    HorizonAck, HorizonCut, HorizonLog, HorizonPeer, HorizonStatement, SignedHorizon,
};

pub(super) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_horizon_bootstrap(scope BLOB PRIMARY KEY,statement BLOB NOT NULL) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_mergeable_horizon(scope BLOB PRIMARY KEY,statement BLOB NOT NULL) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_horizon_pending(dependency BLOB NOT NULL,scope BLOB NOT NULL,PRIMARY KEY(dependency,scope)) WITHOUT ROWID",
    "CREATE INDEX IF NOT EXISTS secure_horizon_pending_scope ON secure_horizon_pending(scope)",
    "CREATE TABLE IF NOT EXISTS secure_horizon_credentials(node BLOB PRIMARY KEY,credentials BLOB NOT NULL) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_horizon_rejoined(scope BLOB NOT NULL,node BLOB NOT NULL,horizon BLOB NOT NULL,PRIMARY KEY(scope,node)) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_horizon_ack(scope BLOB NOT NULL,node BLOB NOT NULL,ack BLOB NOT NULL,PRIMARY KEY(scope,node)) WITHOUT ROWID",
];

/// Required devices still awaited or expired at the statement's fixed deadline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HorizonProgress {
    pub acknowledged: Vec<PublicKey>,
    pub waiting: Vec<PublicKey>,
    pub behind: Vec<PublicKey>,
}

impl<A: RowApplication> Inner<A> {
    #[cfg(test)]
    fn horizon_fault(&mut self, step: u8) -> Result<(), Error> {
        if self.fail_horizon_at == Some(step) {
            self.fail_horizon_at = None;
            return Err(Error::InjectedFailure);
        }
        Ok(())
    }
    pub(crate) async fn mergeable_horizon(
        &self,
        scope: ResourceScope,
    ) -> Result<Option<Box<SignedHorizon>>, Error> {
        let mut found = self
            .conn
            .query(
                "SELECT statement FROM secure_mergeable_horizon WHERE scope=?",
                [encode(&scope)?],
            )
            .await?;
        found
            .next()
            .await?
            .map(|row| {
                SignedHorizon::from_bytes(&row.get::<Vec<u8>>(0)?)
                    .map(Box::new)
                    .map_err(credential)
            })
            .transpose()
    }
    async fn horizon_logs(&self, scope: ResourceScope) -> Result<Vec<HorizonLog>, Error> {
        let Some(id) = self.find_scope_id(scope).await? else {
            return Ok(Vec::new());
        };
        let logs = self.logs(id, None, false, MAX_CHECKPOINT_LOGS + 1).await?;
        if logs.len() > MAX_CHECKPOINT_LOGS {
            return Err(Error::Quota {
                resource: "horizon logs",
                limit: MAX_CHECKPOINT_LOGS,
            });
        }
        Ok(logs
            .into_iter()
            .map(|(log, _)| HorizonLog {
                signer: log.author,
                sequence: log.sequence,
                head: *log.hash.as_bytes(),
            })
            .collect())
    }
    async fn horizon_state(&self, scope: ResourceScope) -> Result<Commitment, Error> {
        match self.find_scope_id(scope).await? {
            Some(id) => Ok(self.scope_state(id).await?.2.digest()),
            None => Ok(checkpoint::Sets::new().rows_summary().digest),
        }
    }
    /// Transitive row dependencies stop at a logged parent: its liveness
    /// is independent of the parent's readable values and constraint winners.
    fn horizon_dependencies(
        &self,
        scope: ResourceScope,
    ) -> Result<(Vec<ResourceScope>, Vec<ResourceScope>), Error> {
        let typed = self
            .typed
            .as_ref()
            .ok_or_else(|| denied("horizon needs installed typed tables"))?;
        let projected = typed
            .by_scope(scope)
            .ok_or_else(|| denied("horizon scope is not installed"))?;
        if projected.confirmed() {
            return Err(denied("confirmed tables use the strong horizon"));
        }
        let schema = typed.rows.schema();
        let mut queue = vec![projected.table.sql_name.clone()];
        let mut seen = BTreeSet::new();
        let mut related = BTreeSet::new();
        let mut existence = BTreeSet::new();
        while let Some(name) = queue.pop() {
            if !seen.insert(name.clone()) {
                continue;
            }
            let table = schema
                .table(&name)
                .ok_or_else(|| denied("missing horizon table"))?;
            let current = if name == projected.table.sql_name {
                scope
            } else {
                let parent = typed
                    .by_name(&name)
                    .ok_or_else(|| denied("missing horizon scope"))?;
                if table.partition_by.is_empty() {
                    parent.scope
                } else {
                    let source = schema
                        .table(&projected.table.sql_name)
                        .ok_or_else(|| denied("missing horizon source"))?;
                    let tuple = self
                        .partitions
                        .get(&scope)
                        .ok_or_else(|| denied("missing horizon partition"))?;
                    let mut values = vec![SqlValue::Null; table.columns.len()];
                    for (from, to) in source.partition_by.iter().zip(&table.partition_by) {
                        let column = source
                            .column(from)
                            .ok_or_else(|| denied("missing horizon partition column"))?;
                        let value = tuple
                            .values()
                            .get(&schema.field_id(source, column))
                            .ok_or_else(|| denied("missing horizon partition value"))?
                            .value()
                            .map_err(|error| denied(&error.to_string()))?;
                        let index = table
                            .columns
                            .iter()
                            .position(|column| column.sql_name.eq_ignore_ascii_case(to))
                            .ok_or_else(|| denied("missing parent partition column"))?;
                        values[index] = value;
                    }
                    let parent_scope = typed.row_scope(parent, &values)?;
                    if !self.partitions.contains_key(&parent_scope)
                        || !self.opening_keys().any(|key| key.scope == parent_scope)
                    {
                        return Err(denied(
                            "hold the parent partition before declaring a horizon",
                        ));
                    }
                    parent_scope
                }
            };
            if table.existence_log {
                existence.insert(current);
            }
            if name != projected.table.sql_name && table.existence_log {
                continue;
            }
            if name != projected.table.sql_name {
                related.insert(current);
            }
            queue.extend(
                table
                    .foreign_keys
                    .iter()
                    .map(|reference| reference.table.clone()),
            );
        }
        Ok((
            related.into_iter().collect(),
            existence.into_iter().collect(),
        ))
    }
    async fn horizon_existence_for(
        &self,
        scope: ResourceScope,
        finalizing: Option<ResourceScope>,
    ) -> Result<HorizonCut, Error> {
        let base = self.existence_base(scope).await?;
        let mut logs = Vec::new();
        for (signer, sequence) in self.existence_frontier(scope).await? {
            let mut found = self.conn.query("SELECT hash FROM secure_existence_log WHERE scope=? AND signer=? AND sequence=?", bindings(vec![SqlValue::Blob(encode(&scope)?), SqlValue::Blob(signer.as_bytes().to_vec()), SqlValue::Integer(sequence as i64)])).await?;
            let head = match found.next().await? {
                Some(row) => checkpoint::row_of(&row.get::<Vec<u8>>(0)?)?,
                None => base
                    .as_ref()
                    .map(|base| base.cut())
                    .transpose()?
                    .and_then(|cut| {
                        cut.logs
                            .iter()
                            .find(|log| log.signer == signer && log.sequence == sequence)
                    })
                    .map(|log| log.head)
                    .ok_or_else(|| Error::Corrupt("existence base head unavailable".into()))?,
            };
            logs.push(HorizonLog {
                signer,
                sequence,
                head,
            });
        }
        let state = rocky_core::commitment(
            "rocky.horizon-existence-state.v2",
            &encode(
                &self
                    .capture_horizon_existence_base(scope, finalizing)
                    .await?,
            )?,
        );
        Ok(HorizonCut { scope, logs, state })
    }
    fn verify_horizon(&self, signed: &SignedHorizon) -> Result<(), Error> {
        signed.verify(self.config.publisher).map_err(credential)?;
        let statement = &signed.statement;
        if statement.manifest != self.manifest
            || statement.policy_epoch != self.config.policy.statement.epoch
        {
            return Err(denied("horizon manifest or policy is not current"));
        }
        let (related, existence) = self.horizon_dependencies(statement.scope)?;
        if existence
            != statement
                .existence
                .iter()
                .map(|cut| cut.scope)
                .collect::<Vec<_>>()
            || related
                != statement
                    .related
                    .iter()
                    .map(|cut| cut.scope)
                    .collect::<Vec<_>>()
        {
            return Err(denied("horizon omits or adds an existence log"));
        }
        Ok(())
    }
    async fn check_horizon_cut(&self, statement: &HorizonStatement) -> Result<(), Error> {
        self.scope_key(statement.scope)?;
        if self.horizon_logs(statement.scope).await? != statement.logs
            || self.horizon_state(statement.scope).await? != statement.state
        {
            return Err(denied(
                "sync to the horizon cut or rebootstrap before acknowledging",
            ));
        }
        for cut in &statement.related {
            if self.typed.as_ref().is_some_and(|typed| {
                typed
                    .by_scope(cut.scope)
                    .is_some_and(|table| typed.partitioned(table))
            }) && !self.holds(cut.scope).await?
            {
                return Err(denied(
                    "hold the parent partition before acknowledging a horizon",
                ));
            }
            if self.horizon_logs(cut.scope).await? != cut.logs
                || self.horizon_state(cut.scope).await? != cut.state
            {
                return Err(denied(
                    "sync parent rows to the horizon cut before acknowledging",
                ));
            }
        }
        for cut in &statement.existence {
            let actual = self
                .horizon_existence_for(cut.scope, Some(statement.scope))
                .await?;
            if actual != *cut {
                return Err(denied(
                    "sync referenced existence logs to the horizon cut before acknowledging",
                ));
            }
        }
        Ok(())
    }
    pub(crate) async fn horizon_progress_at(
        &self,
        signed: &SignedHorizon,
        time: u64,
    ) -> Result<HorizonProgress, Error> {
        let scope = signed.statement.scope;
        let mut progress = HorizonProgress {
            acknowledged: Vec::new(),
            waiting: Vec::new(),
            behind: Vec::new(),
        };
        for peer in &signed.statement.peers {
            let mut rows = self
                .conn
                .query(
                    "SELECT ack FROM secure_horizon_ack WHERE scope=? AND node=?",
                    [encode(&scope)?, peer.node.as_bytes().to_vec()],
                )
                .await?;
            if let Some(row) = rows.next().await? {
                let ack: HorizonAck = decode(&row.get::<Vec<u8>>(0)?)?;
                ack.verify().map_err(credential)?;
                if ack.node != peer.node || ack.horizon != signed.id().map_err(credential)? {
                    return Err(Error::Corrupt("horizon acknowledgment context".into()));
                }
                progress.acknowledged.push(peer.node);
            } else if time >= peer.deadline(signed.statement.issued) {
                progress.behind.push(peer.node);
            } else {
                progress.waiting.push(peer.node);
            }
        }
        Ok(progress)
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// Captures a proposal for signing by the domain issuer outside the DB transaction.
    /// Peers' expiry bounds come from currently verified credentials, never caller-supplied deadlines.
    pub async fn horizon_statement(
        &self,
        scope: ResourceScope,
        peers: &[(PublicKey, CredentialChain)],
    ) -> Result<HorizonStatement, Error> {
        let inner = self.inner.lock().await;
        let time = now()?;
        if peers.len() > 64 {
            return Err(Error::Quota {
                resource: "horizon peers",
                limit: 64,
            });
        }
        inner.scope_key(scope)?;
        let (parents, scopes) = inner.horizon_dependencies(scope)?;
        let mut required = BTreeMap::new();
        for (node, credentials) in peers {
            inner.authorize_peer(*node, credentials, scope)?;
            for parent in &parents {
                inner.authorize_peer(*node, credentials, *parent)?;
            }
            if credentials
                .0
                .last()
                .is_none_or(|link| link.capability.kind != rocky_core::PrincipalKind::Node)
            {
                return Err(denied("horizon participant must be a device"));
            }
            let expires = credentials
                .0
                .iter()
                .map(|link| link.capability.expires)
                .min()
                .ok_or_else(|| denied("horizon peer credentials missing"))?;
            if required
                .insert(
                    *node,
                    HorizonPeer {
                        node: *node,
                        expires,
                    },
                )
                .is_some()
            {
                return Err(denied("duplicate horizon peer"));
            }
        }
        let previous = inner.mergeable_horizon(scope).await?;
        let mut related = Vec::new();
        for scope in parents {
            if inner.typed.as_ref().is_some_and(|typed| {
                typed
                    .by_scope(scope)
                    .is_some_and(|table| typed.partitioned(table))
            }) && !inner.holds(scope).await?
            {
                return Err(denied(
                    "hold the parent partition before declaring a horizon",
                ));
            }
            related.push(HorizonCut {
                scope,
                logs: inner.horizon_logs(scope).await?,
                state: inner.horizon_state(scope).await?,
            });
        }
        let mut existence = Vec::new();
        let mut bases = Vec::new();
        for dependency in scopes {
            existence.push(inner.horizon_existence_for(dependency, Some(scope)).await?);
            bases.push(
                inner
                    .capture_horizon_existence_base(dependency, Some(scope))
                    .await?,
            );
        }
        Ok(HorizonStatement {
            format: rocky_credentials::HORIZON_FORMAT,
            manifest: inner.manifest,
            policy_epoch: inner.config.policy.statement.epoch,
            scope,
            sequence: match &previous {
                Some(signed) => signed
                    .statement
                    .sequence
                    .checked_add(1)
                    .ok_or_else(|| denied("horizon sequence exhausted"))?,
                None => 1,
            },
            previous: previous
                .map(|signed| signed.id().map_err(credential))
                .transpose()?,
            issued: time,
            logs: inner.horizon_logs(scope).await?,
            state: inner.horizon_state(scope).await?,
            existence,
            bases,
            related,
            peers: required.into_values().collect(),
        })
    }
    /// Stores a verified proposal. This does not finalize rows or release history.
    pub async fn receive_horizon(&self, signed: SignedHorizon) -> Result<(), Error> {
        let mut inner = self.inner.lock().await;
        inner.verify_horizon(&signed)?;
        if signed.statement.issued > now()? {
            return Err(denied("horizon issue time is in the future"));
        }
        transaction!(inner, |inner| {
            if let Some(previous) = inner.mergeable_horizon(signed.statement.scope).await? {
                if previous.as_ref() == &signed {
                    return Ok(());
                }
                if signed.statement.sequence
                    != previous
                        .statement
                        .sequence
                        .checked_add(1)
                        .ok_or_else(|| denied("horizon sequence exhausted"))?
                    || signed.statement.previous != Some(previous.id().map_err(credential)?)
                {
                    return Err(denied("horizon does not extend the admitted statement"));
                }
            } else if signed.statement.sequence != 1 {
                return Err(denied("missing horizon predecessor"));
            }
            inner.refuse_horizon_ahead(&signed.statement).await?;
            inner.index_pending_horizon(&signed, true).await?;
            inner.conn.execute("INSERT INTO secure_mergeable_horizon VALUES(?,?) ON CONFLICT(scope) DO UPDATE SET statement=excluded.statement", [encode(&signed.statement.scope)?, signed.to_bytes().map_err(credential)?]).await?;
            inner
                .conn
                .execute(
                    "DELETE FROM secure_horizon_ack WHERE scope=?",
                    [encode(&signed.statement.scope)?],
                )
                .await?;
            Ok(())
        })?;
        inner.flush()?;
        Ok(())
    }
    /// Acknowledges only after rows and referenced existence logs match the cut.
    pub async fn acknowledge_horizon(&self, scope: ResourceScope) -> Result<HorizonAck, Error> {
        let mut inner = self.inner.lock().await;
        let signed = inner
            .mergeable_horizon(scope)
            .await?
            .ok_or_else(|| denied("no horizon statement"))?;
        inner.verify_horizon(&signed)?;
        inner.check_horizon_cut(&signed.statement).await?;
        let ack = HorizonAck::sign(signed.id().map_err(credential)?, &inner.config.node)
            .map_err(credential)?;
        transaction!(inner, |inner| inner.store_horizon_ack(&signed, &ack).await)?;
        inner.flush()?;
        Ok(ack)
    }
    pub async fn receive_horizon_ack(
        &self,
        scope: ResourceScope,
        ack: HorizonAck,
    ) -> Result<(), Error> {
        let mut inner = self.inner.lock().await;
        let signed = inner
            .mergeable_horizon(scope)
            .await?
            .ok_or_else(|| denied("no horizon statement"))?;
        inner.verify_horizon(&signed)?;
        transaction!(inner, |inner| inner.store_horizon_ack(&signed, &ack).await)?;
        inner.flush()?;
        Ok(())
    }
    /// Reports the persisted acknowledgment set and fixed expiry deadlines.
    /// Behind here is a protocol verdict, not permission to prune.
    pub async fn horizon_progress(&self, scope: ResourceScope) -> Result<HorizonProgress, Error> {
        let inner = self.inner.lock().await;
        let signed = inner
            .mergeable_horizon(scope)
            .await?
            .ok_or_else(|| denied("no horizon statement"))?;
        inner.verify_horizon(&signed)?;
        inner.horizon_progress_at(&signed, now()?).await
    }
}
impl<A: RowApplication> Inner<A> {
    async fn store_horizon_ack(
        &self,
        signed: &SignedHorizon,
        ack: &HorizonAck,
    ) -> Result<(), Error> {
        ack.verify().map_err(credential)?;
        if ack.horizon != signed.id().map_err(credential)?
            || !signed
                .statement
                .peers
                .iter()
                .any(|peer| peer.node == ack.node)
        {
            return Err(denied(
                "acknowledgment is outside the horizon participant set",
            ));
        }
        let peer = signed
            .statement
            .peers
            .iter()
            .find(|peer| peer.node == ack.node)
            .ok_or_else(|| denied("unknown horizon participant"))?;
        let mut held = self
            .conn
            .query(
                "SELECT 1 FROM secure_horizon_ack WHERE scope=? AND node=?",
                [
                    encode(&signed.statement.scope)?,
                    ack.node.as_bytes().to_vec(),
                ],
            )
            .await?;
        if held.next().await?.is_none() && now()? >= peer.deadline(signed.statement.issued) {
            return Err(denied(
                "device is behind the horizon deadline; rebootstrap is required",
            ));
        }
        self.conn
            .execute(
                "INSERT INTO secure_horizon_ack VALUES(?,?,?) ON CONFLICT(scope,node) DO NOTHING",
                [
                    encode(&signed.statement.scope)?,
                    ack.node.as_bytes().to_vec(),
                    encode(ack)?,
                ],
            )
            .await?;
        Ok(())
    }
}

impl<A: RowApplication> Inner<A> {
    pub(crate) fn verify_checkpoint_horizon(
        &self,
        checkpoint: &SignedCheckpoint,
    ) -> Result<(), Error> {
        if let Some(finalized) = &checkpoint.statement.finalized
            && (finalized.statement.format != 2
                || checkpoint.statement.horizon.as_ref().is_none_or(|current| {
                    current.statement.sequence < finalized.statement.sequence
                }))
        {
            return Err(denied(
                "finalized checkpoint lacks authenticated existence bases",
            ));
        }
        for signed in checkpoint
            .statement
            .horizon
            .iter()
            .chain(checkpoint.statement.finalized.iter())
        {
            signed.verify(self.config.publisher).map_err(credential)?;
            let horizon = &signed.statement;
            let statement = &checkpoint.statement;
            if horizon.scope != statement.scope
                || horizon.manifest != statement.definition
                || horizon.policy_epoch > statement.policy_epoch
                || horizon.logs.iter().any(|log| {
                    !statement.frontier.iter().any(|position| {
                        position.author == log.signer
                            && position.sequence >= log.sequence
                            && (position.sequence != log.sequence
                                || position.hash.as_bytes() == &log.head)
                    })
                })
            {
                return Err(denied("checkpoint does not cover its issuer horizon"));
            }
            let (related, existence) = self.horizon_dependencies(statement.scope)?;
            if related
                != horizon
                    .related
                    .iter()
                    .map(|cut| cut.scope)
                    .collect::<Vec<_>>()
                || existence
                    != horizon
                        .existence
                        .iter()
                        .map(|cut| cut.scope)
                        .collect::<Vec<_>>()
            {
                return Err(denied("checkpoint horizon dependency mismatch"));
            }
        }
        Ok(())
    }
    pub(crate) async fn check_checkpoint_horizon(
        &self,
        checkpoint: &SignedCheckpoint,
    ) -> Result<(), Error> {
        self.verify_checkpoint_horizon(checkpoint)?;
        self.check_horizon_bootstrap(checkpoint).await?;
        if let Some(id) = self.find_scope_id(checkpoint.statement.scope).await?
            && let Some(record) = self.checkpoint_record(id, true).await?
            && let Some(held) = record.signed.and_then(|signed| signed.statement.finalized)
            && checkpoint
                .statement
                .finalized
                .as_ref()
                .is_none_or(|incoming| {
                    incoming.statement.sequence < held.statement.sequence
                        || (incoming.statement.sequence == held.statement.sequence
                            && incoming != &held)
                })
        {
            return Err(denied("checkpoint would undo finalized horizon state"));
        }
        if let Some(held) = self.mergeable_horizon(checkpoint.statement.scope).await? {
            if held.statement.logs.iter().any(|log| {
                !checkpoint.statement.frontier.iter().any(|position| {
                    position.author == log.signer
                        && position.sequence >= log.sequence
                        && (position.sequence != log.sequence
                            || position.hash.as_bytes() == &log.head)
                })
            }) {
                return Err(denied(
                    "checkpoint is behind the admitted issuer horizon cut",
                ));
            }
            let incoming = checkpoint
                .statement
                .horizon
                .as_ref()
                .ok_or_else(|| denied("checkpoint would lose the admitted issuer horizon"))?;
            let newer_finalized_cut = incoming.statement.sequence > held.statement.sequence
                && checkpoint.statement.finalized.as_ref() == Some(incoming);
            if incoming != &held
                && !newer_finalized_cut
                && (incoming.statement.previous != Some(held.id().map_err(credential)?)
                    || incoming.statement.sequence
                        != held
                            .statement
                            .sequence
                            .checked_add(1)
                            .ok_or_else(|| denied("horizon sequence exhausted"))?)
            {
                return Err(denied(
                    "checkpoint horizon does not extend the admitted declaration",
                ));
            }
        }
        Ok(())
    }
    pub(crate) async fn adopt_checkpoint_horizon(
        &self,
        incoming: &SignedHorizon,
    ) -> Result<(), Error> {
        if self
            .mergeable_horizon(incoming.statement.scope)
            .await?
            .as_deref()
            != Some(incoming)
        {
            self.conn.execute("INSERT INTO secure_mergeable_horizon VALUES(?,?) ON CONFLICT(scope) DO UPDATE SET statement=excluded.statement",
                [encode(&incoming.statement.scope)?, incoming.to_bytes().map_err(credential)?]).await?;
            self.conn
                .execute(
                    "DELETE FROM secure_horizon_ack WHERE scope=?",
                    [encode(&incoming.statement.scope)?],
                )
                .await?;
        }
        Ok(())
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    pub async fn record_horizon_credentials(
        &self,
        node: PublicKey,
        credentials: &CredentialChain,
    ) -> Result<(), Error> {
        let mut inner = self.inner.lock().await;
        inner.verify_chain_scopes(credentials, node, now()?, &[])?;
        let bytes = encode(credentials)?;
        let mut rows = inner
            .conn
            .query(
                "SELECT credentials FROM secure_horizon_credentials WHERE node=?",
                [node.as_bytes().to_vec()],
            )
            .await?;
        if let Some(row) = rows.next().await?
            && row.get::<Vec<u8>>(0)? == bytes
        {
            return Ok(());
        }
        transaction!(inner, |inner| {
            inner.conn.execute("INSERT INTO secure_horizon_credentials VALUES(?,?) ON CONFLICT(node) DO UPDATE SET credentials=excluded.credentials", [node.as_bytes().to_vec(),bytes.clone()]).await?;
            Ok(())
        })
    }
    pub async fn horizon_credentials(
        &self,
        nodes: &[PublicKey],
    ) -> Result<Vec<(PublicKey, CredentialChain)>, Error> {
        let inner = self.inner.lock().await;
        let mut result = Vec::new();
        for node in nodes {
            let mut rows = inner
                .conn
                .query(
                    "SELECT credentials FROM secure_horizon_credentials WHERE node=?",
                    [node.as_bytes().to_vec()],
                )
                .await?;
            let row = rows.next().await?.ok_or_else(|| {
                denied("sync with the required device before declaring its credential deadline")
            })?;
            result.push((*node, decode(&row.get::<Vec<u8>>(0)?)?));
        }
        Ok(result)
    }
    pub async fn admitted_horizon(
        &self,
        scope: ResourceScope,
    ) -> Result<Option<Box<SignedHorizon>>, Error> {
        self.inner.lock().await.mergeable_horizon(scope).await
    }
    pub async fn finalized_horizon(
        &self,
        scope: ResourceScope,
    ) -> Result<Option<Box<SignedHorizon>>, Error> {
        let inner = self.inner.lock().await;
        let Some(id) = inner.find_scope_id(scope).await? else {
            return Ok(None);
        };
        Ok(inner
            .checkpoint_record(id, true)
            .await?
            .and_then(|record| record.signed)
            .and_then(|signed| signed.statement.finalized))
    }
    pub async fn horizon_operations_since_finalization(
        &self,
        scope: ResourceScope,
    ) -> Result<u64, Error> {
        let inner = self.inner.lock().await;
        let finalized = match inner.find_scope_id(scope).await? {
            Some(id) => inner
                .checkpoint_record(id, true)
                .await?
                .and_then(|record| record.signed)
                .and_then(|signed| signed.statement.finalized),
            None => None,
        };
        Ok(inner
            .horizon_logs(scope)
            .await?
            .iter()
            .map(|log| {
                let covered = finalized
                    .as_ref()
                    .and_then(|signed| {
                        signed
                            .statement
                            .logs
                            .iter()
                            .find(|cut| cut.signer == log.signer)
                    })
                    .map_or(0, |cut| cut.sequence.saturating_add(1));
                log.sequence.saturating_add(1).saturating_sub(covered)
            })
            .sum())
    }
    pub async fn horizon_rebootstrap_required(&self, scope: ResourceScope) -> Result<bool, Error> {
        let inner = self.inner.lock().await;
        inner
            .horizon_device_behind(scope, inner.config.node.public_key())
            .await
    }
    pub async fn acknowledge_horizon_rebootstrap(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scope: ResourceScope,
        horizon: Commitment,
    ) -> Result<(), Error> {
        let mut inner = self.inner.lock().await;
        inner.authorize_peer(peer, credentials, scope)?;
        let signed = inner
            .mergeable_horizon(scope)
            .await?
            .ok_or_else(|| denied("no horizon for rebootstrap receipt"))?;
        let id = inner.scope_id(scope).await?;
        if signed.id().map_err(credential)? != horizon
            || inner
                .checkpoint_record(id, true)
                .await?
                .and_then(|record| record.signed)
                .is_none_or(|checkpoint| checkpoint.statement.finalized.as_ref() != Some(&signed))
        {
            return Err(denied(
                "rebootstrap receipt does not name the finalized issuer cut",
            ));
        }
        transaction!(inner, |inner| {
            inner.conn.execute("INSERT INTO secure_horizon_rejoined VALUES(?,?,?) ON CONFLICT(scope,node) DO UPDATE SET horizon=excluded.horizon", [encode(&scope)?, peer.as_bytes().to_vec(), horizon.to_vec()]).await?;
            Ok(())
        })?;
        inner.flush()?;
        Ok(())
    }
    pub async fn horizon_peer_behind(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scope: ResourceScope,
    ) -> Result<bool, Error> {
        let inner = self.inner.lock().await;
        inner.authorize_peer(peer, credentials, scope)?;
        inner.horizon_device_behind(scope, peer).await
    }
}
impl<A: RowApplication> Inner<A> {
    pub(super) async fn horizon_device_behind(
        &self,
        scope: ResourceScope,
        node: PublicKey,
    ) -> Result<bool, Error> {
        if node == self.config.node.public_key() && self.horizon_bootstrap(scope).await?.is_some() {
            return Ok(true);
        }
        let Some(signed) = self.mergeable_horizon(scope).await? else {
            return Ok(false);
        };
        if node == self.config.node.public_key()
            && let Some(id) = self.find_scope_id(scope).await?
            && self
                .checkpoint_record(id, true)
                .await?
                .and_then(|record| record.signed)
                .is_some_and(|checkpoint| checkpoint.statement.finalized.as_ref() == Some(&signed))
        {
            return Ok(false);
        }
        let mut rejoined = self
            .conn
            .query(
                "SELECT horizon FROM secure_horizon_rejoined WHERE scope=? AND node=?",
                [encode(&scope)?, node.as_bytes().to_vec()],
            )
            .await?;
        if let Some(row) = rejoined.next().await?
            && row.get::<Vec<u8>>(0)? == signed.id().map_err(credential)?.to_vec()
        {
            return Ok(false);
        }
        Ok(self
            .horizon_progress_at(&signed, now()?)
            .await?
            .behind
            .contains(&node))
    }
    pub(in crate::replica) async fn horizon_copies(
        &self,
        signed: &SignedHorizon,
    ) -> Result<usize, Error> {
        let mut peers: BTreeSet<_> = self
            .horizon_progress_at(signed, now()?)
            .await?
            .acknowledged
            .into_iter()
            .collect();
        for row in collect_rows(
            self.conn
                .query(
                    "SELECT node FROM secure_horizon_rejoined WHERE scope=? AND horizon=?",
                    [
                        encode(&signed.statement.scope)?,
                        signed.id().map_err(credential)?.to_vec(),
                    ],
                )
                .await?,
        )
        .await?
        {
            let [SqlValue::Blob(node)] = row.as_slice() else {
                return Err(Error::Corrupt("horizon holder".into()));
            };
            peers.insert(checkpoint::key_of(node)?);
        }
        peers.remove(&self.config.node.public_key());
        Ok(peers.len())
    }
    pub(crate) async fn index_pending_horizon(
        &self,
        signed: &SignedHorizon,
        pending: bool,
    ) -> Result<(), Error> {
        let scope = encode(&signed.statement.scope)?;
        self.conn
            .execute(
                "DELETE FROM secure_horizon_pending WHERE scope=?",
                [scope.clone()],
            )
            .await?;
        if pending {
            for dependency in std::iter::once(signed.statement.scope).chain(
                signed
                    .statement
                    .related
                    .iter()
                    .chain(&signed.statement.existence)
                    .map(|cut| cut.scope),
            ) {
                self.conn
                    .execute(
                        "INSERT OR IGNORE INTO secure_horizon_pending VALUES(?,?)",
                        [encode(&dependency)?, scope.clone()],
                    )
                    .await?;
            }
        }
        Ok(())
    }
    async fn pending_horizons(&self, scope: ResourceScope) -> Result<Vec<SignedHorizon>, Error> {
        let mut pending = Vec::new();
        for row in collect_rows(self.conn.query(
            "SELECT h.statement FROM secure_horizon_pending p JOIN secure_mergeable_horizon h ON h.scope=p.scope WHERE p.dependency=?",
            [encode(&scope)?],
        ).await?).await? {
            let [SqlValue::Blob(bytes)] = row.as_slice() else {
                return Err(Error::Corrupt("horizon statement".into()));
            };
            pending.push(SignedHorizon::from_bytes(bytes).map_err(credential)?);
        }
        Ok(pending)
    }
    pub(super) async fn ensure_horizon_writable(&self, scope: ResourceScope) -> Result<(), Error> {
        self.ensure_horizon_current(scope).await?;
        for signed in self.pending_horizons(scope).await? {
            if signed.statement.scope == scope
                || signed
                    .statement
                    .related
                    .iter()
                    .chain(&signed.statement.existence)
                    .any(|cut| cut.scope == scope)
            {
                return Err(denied(
                    "horizon cut is sealed; finalize or rebootstrap before writing",
                ));
            }
        }
        Ok(())
    }
    pub(super) async fn horizon_allows_position(
        &self,
        scope: ResourceScope,
        signer: PublicKey,
        sequence: u64,
        existence: bool,
    ) -> Result<bool, Error> {
        for signed in self.pending_horizons(scope).await? {
            let logs = if existence {
                signed
                    .statement
                    .existence
                    .iter()
                    .find(|cut| cut.scope == scope)
                    .map(|cut| &cut.logs)
            } else if signed.statement.scope == scope {
                Some(&signed.statement.logs)
            } else {
                signed
                    .statement
                    .related
                    .iter()
                    .find(|cut| cut.scope == scope)
                    .map(|cut| &cut.logs)
            };
            if let Some(logs) = logs
                && !logs
                    .iter()
                    .any(|log| log.signer == signer && sequence <= log.sequence)
            {
                return Ok(false);
            }
        }
        Ok(true)
    }
    async fn refuse_horizon_ahead(&self, statement: &HorizonStatement) -> Result<(), Error> {
        for (scope, logs) in std::iter::once((statement.scope, &statement.logs))
            .chain(statement.related.iter().map(|cut| (cut.scope, &cut.logs)))
        {
            for held in self.horizon_logs(scope).await? {
                if !logs.iter().any(|cut| {
                    cut.signer == held.signer
                        && cut.sequence >= held.sequence
                        && (cut.sequence != held.sequence || cut.head == held.head)
                }) {
                    return Err(denied(
                        "holder is ahead of the declared cut; import its finalized checkpoint",
                    ));
                }
            }
        }
        for cut in &statement.existence {
            for (signer, sequence) in self.existence_frontier(cut.scope).await? {
                if !cut
                    .logs
                    .iter()
                    .any(|log| log.signer == signer && log.sequence >= sequence)
                {
                    return Err(denied(
                        "existence holder is ahead of the cut; import its finalized checkpoint",
                    ));
                }
            }
        }
        Ok(())
    }
    pub(super) async fn ensure_horizon_current(&self, scope: ResourceScope) -> Result<(), Error> {
        if self
            .horizon_device_behind(scope, self.config.node.public_key())
            .await?
        {
            return Err(denied(
                "device is behind the horizon; phase-3 rebootstrap is required before sync or write",
            ));
        }
        Ok(())
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    pub async fn installed_existence_base(
        &self,
        scope: ResourceScope,
    ) -> Result<Option<ExistenceBaseAttachment>, Error> {
        self.inner.lock().await.existence_base(scope).await
    }
}

impl<A: RowApplication> Inner<A> {
    pub(super) async fn finalized_basis(
        &self,
        scope: ResourceScope,
    ) -> Result<Option<(u64, Commitment)>, Error> {
        let Some(id) = self.find_scope_id(scope).await? else {
            return Ok(None);
        };
        self.checkpoint_record(id, true)
            .await?
            .and_then(|record| record.signed)
            .and_then(|signed| signed.statement.finalized)
            .map(|signed| Ok((signed.statement.sequence, signed.id().map_err(credential)?)))
            .transpose()
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// Finalizes an acknowledged exact cut and replaces its replay base atomically.
    /// Existence histories remain retained until an authenticated replacement is installed.
    pub async fn finalize_horizon(&self, scope: ResourceScope) -> Result<SignedCheckpoint, Error> {
        // First prove retained replay; the final transaction rechecks the exact input.
        self.checkpoint(scope).await?;
        let mut inner = self.inner.lock().await;
        inner.settle(scope).await?;
        let signed = inner
            .mergeable_horizon(scope)
            .await?
            .ok_or_else(|| denied("no horizon statement"))?;
        inner.verify_horizon(&signed)?;
        inner.ensure_horizon_current(scope).await?;
        let id = inner.scope_id(scope).await?;
        let previous = inner
            .checkpoint_record(id, true)
            .await?
            .ok_or_else(|| denied("no verified checkpoint"))?;
        let previous_signed = inner.verify_record(scope, &previous)?;
        if previous_signed.statement.finalized.as_ref() == Some(&signed) {
            return Ok(previous_signed);
        }
        let result = transaction!(inner, |inner| {
            inner.check_horizon_cut(&signed.statement).await?;
            if !inner
                .horizon_progress_at(&signed, now()?)
                .await?
                .waiting
                .is_empty()
            {
                return Err(denied(
                    "horizon participants have not acknowledged or expired",
                ));
            }
            inner.seal_existence_bases(&signed).await?;
            #[cfg(test)]
            inner.horizon_fault(1)?;
            let scope_id = inner.scope_id(scope).await?;
            inner
                .conn
                .execute(
                    "DELETE FROM secure_stubs WHERE scope=? AND creation=0",
                    [scope_id],
                )
                .await?;
            #[cfg(test)]
            inner.horizon_fault(2)?;
            let typed = inner
                .typed
                .clone()
                .ok_or_else(|| denied("horizon requires typed rows"))?;
            let table = typed
                .by_scope(scope)
                .ok_or_else(|| denied("horizon table missing"))?;
            let id_column = typed
                .rows
                .schema()
                .table(&table.table.sql_name)
                .and_then(|table| {
                    table
                        .columns
                        .iter()
                        .position(|column| column.sql_name.eq_ignore_ascii_case("id"))
                })
                .ok_or_else(|| denied("horizon requires row incarnations"))?;
            let blob = encode(&scope)?;
            let mut sets = checkpoint::Sets::new();
            inner
                .conn
                .execute(
                    "DELETE FROM secure_horizon_live_creations WHERE scope=?",
                    [id],
                )
                .await?;
            let mut after = Vec::new();
            loop {
                let rows = collect_rows(inner.conn.query(
                    "SELECT row,fields FROM secure_rows WHERE scope=? AND row>? ORDER BY row LIMIT ?",
                    bindings(vec![SqlValue::Blob(blob.clone()), SqlValue::Blob(after.clone()), SqlValue::Integer(checkpoint::STEP_ROWS as i64)]),
                ).await?).await?;
                if rows.is_empty() {
                    break;
                }
                for row in rows {
                    let [SqlValue::Blob(key), SqlValue::Blob(fields)] = row.as_slice() else {
                        return Err(Error::Corrupt("horizon row".into()));
                    };
                    after = key.clone();
                    let mut record: Materialized = decode(fields)?;
                    let image = if record.deleted {
                        None
                    } else {
                        let values = typed
                            .rows
                            .values(&table.table, &record.fields)
                            .map_err(Error::Corrupt)?;
                        let parameters = table
                            .table
                            .key
                            .iter()
                            .map(|index| values[*index].clone())
                            .collect();
                        collect_rows(
                            inner
                                .conn
                                .query(
                                    if typed
                                        .rows
                                        .schema()
                                        .table(&table.table.sql_name)
                                        .is_some_and(|table| table.column("row_kind").is_some())
                                    {
                                        format!("{} AND row_kind=0", table.select)
                                    } else {
                                        table.select.clone()
                                    },
                                    bindings(parameters),
                                )
                                .await?,
                        )
                        .await?
                        .pop()
                        .filter(|values| {
                            record.identity.is_some_and(|id| {
                                values.get(id_column) == Some(&SqlValue::Integer(id))
                            })
                        })
                    };
                    if let Some(image) = image {
                        inner
                            .conn
                            .execute(
                                "INSERT OR IGNORE INTO secure_horizon_live_creations VALUES(?,?)",
                                bindings(vec![
                                    SqlValue::Integer(id),
                                    SqlValue::Blob(record.creation.as_bytes().to_vec()),
                                ]),
                            )
                            .await?;
                        // Only substituted columns change; existing register stamps survive.
                        let fields = typed
                            .rows
                            .fields(&table.table, &image)
                            .map_err(Error::Corrupt)?;
                        for (field, value) in fields {
                            if record.fields.get(&field) != Some(&value) {
                                record.fields.insert(field, value);
                            }
                        }
                        record.protected = Some(protection::Protected::new(
                            signed.id().map_err(credential)?,
                            record.fields.clone(),
                        ));
                        let new = encode(&record)?;
                        sets.replace(&checkpoint::row_of(key)?, None, &new)?;
                        inner
                            .conn
                            .execute(
                                "UPDATE secure_rows SET fields=? WHERE scope=? AND row=?",
                                [new, blob.clone(), key.clone()],
                            )
                            .await?;
                    } else {
                        inner
                            .conn
                            .execute(
                                "DELETE FROM secure_rows WHERE scope=? AND row=?",
                                [blob.clone(), key.clone()],
                            )
                            .await?;
                    }
                }
            }
            #[cfg(test)]
            inner.horizon_fault(3)?;
            inner.conn.execute("DELETE FROM secure_stubs WHERE scope=? AND creation=1 AND hash NOT IN (SELECT hash FROM secure_horizon_live_creations WHERE scope=?)", [id,id]).await?;
            let mut after = Vec::new();
            loop {
                let stubs = collect_rows(inner.conn.query("SELECT hash,clock FROM secure_stubs WHERE scope=? AND creation=1 AND hash>? ORDER BY hash LIMIT ?",bindings(vec![SqlValue::Integer(id), SqlValue::Blob(after.clone()),SqlValue::Integer(checkpoint::STEP_ROWS as i64)])).await?).await?;
                if stubs.is_empty() {
                    break;
                }
                for stub in stubs {
                    let [SqlValue::Blob(hash), SqlValue::Integer(clock)] = stub.as_slice() else {
                        return Err(Error::Corrupt("horizon creation stub".into()));
                    };
                    sets.create(checkpoint::hash_of(hash)?, *clock as u64);
                    after = hash.clone();
                }
            }
            inner
                .conn
                .execute(
                    "DELETE FROM secure_horizon_live_creations WHERE scope=?",
                    [id],
                )
                .await?;
            inner.reproject(scope).await?;
            inner.index_pending_horizon(&signed, false).await?;
            #[cfg(test)]
            inner.horizon_fault(4)?;
            let mut statement = previous_signed.statement.clone();
            statement.format = 6;
            statement.finalized = Some(signed.clone());
            statement.sequence = statement
                .sequence
                .checked_add(1)
                .ok_or_else(|| denied("checkpoint sequence exhausted"))?;
            statement.previous = Some(previous_signed.id()?);
            inner
                .capture_checkpoint_creations(scope, id, "secure_checkpoint_stubs")
                .await?;
            statement.rows = sets.rows_summary();
            statement.tombstones = 0;
            statement.creations = sets.creations_summary();
            let finalized = SignedCheckpoint::sign(
                statement,
                &inner.config.node,
                inner.config.credentials.clone(),
            )?;
            for sql in [
                "DELETE FROM secure_checkpoint_rows WHERE scope=?",
                "INSERT INTO secure_checkpoint_rows SELECT ?,row,fields FROM secure_rows WHERE scope=(SELECT scope FROM secure_scope_state WHERE id=?)",
            ] {
                if sql.starts_with("INSERT") {
                    inner.conn.execute(sql, [id, id]).await?;
                } else {
                    inner.conn.execute(sql, [id]).await?;
                }
            }
            for log in &signed.statement.logs {
                inner
                    .conn
                    .execute(
                        "UPDATE secure_logs SET horizon=MAX(horizon,?) WHERE scope=? AND author=?",
                        bindings(vec![
                            SqlValue::Integer(log.sequence as i64),
                            SqlValue::Integer(id),
                            SqlValue::Blob(log.signer.as_bytes().to_vec()),
                        ]),
                    )
                    .await?;
            }
            inner
                .conn
                .execute("DELETE FROM secure_checkpoints WHERE scope=?", [id])
                .await?;
            inner
                .conn
                .execute(
                    "INSERT INTO secure_checkpoints VALUES(?,?,'verified',?,?,?,NULL,?)",
                    bindings(vec![
                        SqlValue::Integer(id),
                        SqlValue::Integer(finalized.statement.sequence as i64),
                        SqlValue::Integer(previous.position),
                        SqlValue::Integer(previous.position),
                        SqlValue::Blob(sets.to_bytes()),
                        SqlValue::Blob(finalized.to_bytes()?),
                    ]),
                )
                .await?;
            #[cfg(test)]
            inner.horizon_fault(5)?;
            let applied = inner.scope_state(id).await?.3;
            inner
                .store_scope_state(id, sets.rows, 0, &sets.digest, applied)
                .await?;
            inner
                .conn
                .execute(
                    "UPDATE secure_metadata SET revision=revision+1 WHERE id=1",
                    (),
                )
                .await?;
            #[cfg(test)]
            if inner.fail_before_commit > 0 {
                inner.fail_before_commit -= 1;
                return Err(Error::InjectedFailure);
            }
            Ok(finalized)
        })?;
        let commit = inner.commit;
        inner.verified_at.insert(id, commit);
        inner.flush()?;
        inner.publish_revision().await?;
        inner.process_pending(now()?).await?;
        Ok(result)
    }
}
