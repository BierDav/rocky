use super::*;
use rocky_core::replay::{BaseMark, OutcomeRecord};

const OUTCOMES_TABLE: &str = "CREATE TABLE IF NOT EXISTS secure_outcomes(txn BLOB NOT NULL,publisher BLOB NOT NULL,sequence INTEGER NOT NULL,record BLOB NOT NULL,PRIMARY KEY(txn,publisher)) WITHOUT ROWID";
const OUTCOME_MEMBERS_TABLE: &str = "CREATE TABLE IF NOT EXISTS secure_outcome_members(txn BLOB NOT NULL,member INTEGER NOT NULL,PRIMARY KEY(txn,member)) WITHOUT ROWID";
const OUTCOME_DIVERGENCE_TABLE: &str = "CREATE TABLE IF NOT EXISTS secure_outcome_divergence(txn BLOB NOT NULL,publisher BLOB NOT NULL,stored TEXT NOT NULL,refused TEXT NOT NULL,PRIMARY KEY(txn,publisher)) WITHOUT ROWID";
pub(in crate::replica) const SCHEMA: &[&str] = &[
    OUTCOMES_TABLE,
    OUTCOME_MEMBERS_TABLE,
    OUTCOME_DIVERGENCE_TABLE,
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutcomeDivergence {
    pub transaction: Commitment,
    pub publisher: PublicKey,
    pub stored: String,
    pub refused: String,
}

fn engine() -> String {
    format!(
        "rocky-secure {} {}",
        env!("CARGO_PKG_VERSION"),
        sqlite::engine()
    )
}

impl<A: RowApplication> Inner<A> {
    async fn prefix(&self, scope: i64, hash: Hash) -> Result<Vec<BaseMark>, Error> {
        let prefix = self.prefix_unbounded(scope, hash).await?;
        if prefix.len() > rocky_core::replay::MAX_OUTCOME_MEMBERS {
            return Err(denied("outcome prefix quota"));
        }
        Ok(prefix)
    }

    async fn prefix_unbounded(&self, scope: i64, hash: Hash) -> Result<Vec<BaseMark>, Error> {
        let rows = collect_rows(
            self.conn
                .query(
                    "SELECT author,MAX(sequence) FROM secure_replayed WHERE scope=? AND (era,clock,author,sequence)<(SELECT era,clock,author,sequence FROM secure_replayed WHERE hash=?) GROUP BY author ORDER BY author",
                    bindings(vec![
                        SqlValue::Integer(scope),
                        SqlValue::Blob(hash.as_bytes().to_vec()),
                    ]),
                )
                .await?,
        )
        .await?;
        rows.iter()
            .map(|row| match row.as_slice() {
                [SqlValue::Blob(author), SqlValue::Integer(sequence)] => Ok(BaseMark {
                    author: stored_writer(author)?,
                    sequence: *sequence as u64,
                }),
                _ => Err(Error::Corrupt("replayed prefix".into())),
            })
            .collect()
    }

    async fn linked_prefixes(
        &self,
        held: &[(&EncryptedFrame, u16)],
    ) -> Result<Option<Commitment>, Error> {
        let Some((first, _)) = held.first() else {
            return Ok(None);
        };
        let mut own = BTreeSet::new();
        let mut linked = BTreeSet::new();
        for (frame, _) in held {
            let id = self
                .scope_id(frame.operation_ref()?.header.extensions.scope)
                .await?;
            own.insert(id);
            linked.extend(self.linked_scopes(id).await?);
        }
        let hash = first.operation_ref()?.hash;
        let mut prefixes = Vec::with_capacity(linked.len());
        for id in linked.difference(&own) {
            let rows = collect_rows(
                self.conn
                    .query("SELECT scope FROM secure_scope_state WHERE id=?", [*id])
                    .await?,
            )
            .await?;
            let Some([SqlValue::Blob(scope)]) = rows.first().map(Vec::as_slice) else {
                return Err(Error::Corrupt("replayed scope".into()));
            };
            prefixes.push((scope.clone(), self.prefix_unbounded(*id, hash).await?));
        }
        if prefixes.is_empty() {
            return Ok(None);
        }
        prefixes.sort();
        Ok(Some(rocky_core::commitment(
            "rocky.outcome.linked.v1",
            &encode(&prefixes)?,
        )))
    }

    async fn outcome_records(
        &self,
        transaction: Commitment,
    ) -> Result<Vec<(Vec<u8>, i64, OutcomeRecord)>, Error> {
        collect_rows(
            self.conn
                .query(
                    "SELECT publisher,sequence,record FROM secure_outcomes WHERE txn=?",
                    [transaction.to_vec()],
                )
                .await?,
        )
        .await?
        .into_iter()
        .map(|row| match row.as_slice() {
            [
                SqlValue::Blob(publisher),
                SqlValue::Integer(sequence),
                SqlValue::Blob(record),
            ] => Ok((publisher.clone(), *sequence, decode(record)?)),
            _ => Err(Error::Corrupt("outcome record".into())),
        })
        .collect()
    }

    async fn store_outcome(
        &self,
        publisher: &[u8],
        sequence: u64,
        record: &OutcomeRecord,
    ) -> Result<(), Error> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO secure_outcomes VALUES(?,?,?,?)",
                bindings(vec![
                    SqlValue::Blob(record.transaction.to_vec()),
                    SqlValue::Blob(publisher.to_vec()),
                    SqlValue::Integer(sequence as i64),
                    SqlValue::Blob(encode(record)?),
                ]),
            )
            .await?;
        for (member, _) in &record.replayed {
            self.conn
                .execute(
                    "INSERT OR IGNORE INTO secure_outcome_members VALUES(?,?)",
                    bindings(vec![
                        SqlValue::Blob(record.transaction.to_vec()),
                        SqlValue::Integer(i64::from(*member)),
                    ]),
                )
                .await?;
        }
        Ok(())
    }

    pub(super) async fn remote_drop(
        &self,
        transaction: Commitment,
        held: &[u16],
    ) -> Result<Option<DropReason>, Error> {
        let own = self.config.node.public_key();
        Ok(self
            .outcome_records(transaction)
            .await?
            .into_iter()
            .filter(|(publisher, ..)| publisher.as_slice() != own.as_bytes())
            .filter(|(.., record)| {
                record
                    .replayed
                    .iter()
                    .any(|(member, _)| !held.contains(member))
            })
            .find_map(|(.., record)| record.dropped))
    }

    pub(in crate::replica) async fn publish_outcome(
        &self,
        transaction: Commitment,
        held: &[(&EncryptedFrame, u16)],
        dropped: Option<DropReason>,
        time: u64,
    ) -> Result<(), Error> {
        let Some(node) = self
            .node_author
            .as_ref()
            .map(|author| author.key.public_key())
        else {
            return Ok(());
        };
        let rederive_pending = self.rederive_marked
            && !rows(&self.conn, "SELECT 1 FROM secure_replayed_rederive LIMIT 1")
                .await?
                .is_empty();
        if rederive_pending {
            return Ok(());
        }
        let grants = self.local_grants(time)?;
        let mut replayed = Vec::with_capacity(held.len());
        let mut logs = Vec::new();
        for (frame, member) in held {
            let operation = frame.operation_ref()?;
            let scope = operation.header.extensions.scope;
            let id = self.scope_id(scope).await?;
            replayed.push((*member, self.prefix(id, operation.hash).await?));
            let writes = grants.iter().any(|grant| {
                grant.scope == scope
                    && [Right::Insert, Right::Update, Right::Delete]
                        .iter()
                        .any(|right| grant.rights.contains(right))
            });
            if writes && self.holds(scope).await? && !self.own_pending(scope, node).await? {
                logs.push((scope, id));
            }
        }
        if logs.is_empty() {
            return Ok(());
        }
        replayed.sort_by_key(|(member, _)| *member);
        let unchanged =
            self.outcome_records(transaction)
                .await?
                .iter()
                .any(|(publisher, _, own)| {
                    publisher.as_slice() == node.as_bytes()
                        && own.dropped == dropped
                        && own
                            .replayed
                            .iter()
                            .map(|(member, _)| member)
                            .eq(replayed.iter().map(|(member, _)| member))
                });
        if unchanged {
            return Ok(());
        }
        let linked = self.linked_prefixes(held).await?;
        let record = OutcomeRecord {
            transaction,
            replayed,
            linked,
            dropped,
            engine: engine(),
        };
        self.sign_outcome(&record, &logs, node, time).await
    }

    async fn own_pending(&self, scope: ResourceScope, node: PublicKey) -> Result<bool, Error> {
        Ok(!collect_rows(
            self.conn
                .query(
                    "SELECT 1 FROM secure_operations WHERE scope=? AND author=? AND status='pending' LIMIT 1",
                    [encode(&scope)?, node.as_bytes().to_vec()],
                )
                .await?,
        )
        .await?
        .is_empty())
    }
    async fn sign_outcome(
        &self,
        record: &OutcomeRecord,
        logs: &[(ResourceScope, i64)],
        node: PublicKey,
        time: u64,
    ) -> Result<(), Error> {
        record.check().map_err(denied)?;
        let mut sequence = 0;
        for (scope, id) in logs {
            let tip = self.author_tip(*scope, *id, node).await?;
            let frame = self
                .author_outcome_frame((*scope, tip), time, record)
                .await?;
            sequence = frame.operation_ref()?.header.extensions.author_sequence;
        }
        self.store_outcome(node.as_bytes(), sequence, record).await
    }

    pub(in crate::replica) async fn apply_outcome(
        &self,
        frame: &EncryptedFrame,
        record: &OutcomeRecord,
        time: u64,
    ) -> Result<Result<(), String>, Error> {
        let header = &frame.operation_ref()?.header;
        let publisher = header.public_key;
        let sequence = header.extensions.author_sequence;
        let stored = self.outcome_records(record.transaction).await?;
        if stored.iter().any(|(stored, at, _)| {
            stored.as_slice() == publisher.as_bytes() && *at as u64 >= sequence
        }) {
            return Ok(Ok(()));
        }
        if let Some((.., other)) = stored.iter().find(|(stored, _, other)| {
            stored.as_slice() != publisher.as_bytes()
                && other.replayed == record.replayed
                && other.linked == record.linked
                && other.dropped != record.dropped
        }) {
            tracing::warn!(
                stored = %other.engine,
                refused = %record.engine,
                "divergent outcome records"
            );
            self.conn
                .execute(
                    "INSERT OR REPLACE INTO secure_outcome_divergence VALUES(?,?,?,?)",
                    bindings(vec![
                        SqlValue::Blob(record.transaction.to_vec()),
                        SqlValue::Blob(publisher.as_bytes().to_vec()),
                        SqlValue::Text(other.engine.clone()),
                        SqlValue::Text(record.engine.clone()),
                    ]),
                )
                .await?;
            return Ok(Err(format!(
                "divergent outcome records for one transaction: {} and {}",
                other.engine, record.engine
            )));
        }
        self.store_outcome(publisher.as_bytes(), sequence, record)
            .await?;
        let held = collect_rows(
            self.conn
                .query(
                    "SELECT era,clock,author,sequence,scope,member,dropped FROM secure_replayed WHERE txn=? AND sealed=0",
                    [record.transaction.to_vec()],
                )
                .await?,
        )
        .await?;
        let mut starts = Vec::with_capacity(held.len());
        let mut members = Vec::with_capacity(held.len());
        let mut dropped = false;
        for row in &held {
            let [
                ..,
                SqlValue::Integer(scope),
                SqlValue::Integer(member),
                outcome,
            ] = row.as_slice()
            else {
                return Err(Error::Corrupt("replayed member".into()));
            };
            starts.push((*scope, order_of(row)?));
            members.push(*member as u16);
            dropped |= !matches!(outcome, SqlValue::Null);
        }
        if held.is_empty()
            || self
                .remote_drop(record.transaction, &members)
                .await?
                .is_some()
                == dropped
        {
            return Ok(Ok(()));
        }
        let redo = self.rewind(starts).await?;
        self.redo(redo, time).await?;
        Ok(Ok(()))
    }
}

#[cfg(test)]
impl<A: RowApplication> EncryptedReplica<A> {
    pub(crate) async fn forge_outcome_for_test(
        &self,
        transaction: Commitment,
        dropped: Option<DropReason>,
        engine: &str,
        scopes: &[ResourceScope],
    ) -> Result<(), Error> {
        let mut inner = self.inner.lock(Class::Background).await;
        let time = now()?;
        let engine = engine.to_owned();
        let scopes = scopes.to_vec();
        transaction!(inner, |inner| {
            let node = inner.config.node.public_key();
            let (.., mut record) = inner
                .outcome_records(transaction)
                .await?
                .into_iter()
                .find(|(publisher, ..)| publisher.as_slice() == node.as_bytes())
                .ok_or(Error::Closed)?;
            record.dropped = dropped;
            record.engine = engine.clone();
            let mut logs = Vec::new();
            for scope in &scopes {
                logs.push((*scope, inner.scope_id(*scope).await?));
            }
            inner.sign_outcome(&record, &logs, node, time).await
        })
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    pub async fn outcome_divergences(&self) -> Result<Vec<OutcomeDivergence>, Error> {
        let inner = self.inner.lock(Class::Background).await;
        let corrupt = || Error::Corrupt("outcome divergence".into());
        collect_rows(
            inner
                .conn
                .query(
                    "SELECT txn,publisher,stored,refused FROM secure_outcome_divergence ORDER BY txn,publisher",
                    (),
                )
                .await?,
        )
        .await?
        .into_iter()
        .map(|row| match row.as_slice() {
            [
                SqlValue::Blob(txn),
                SqlValue::Blob(publisher),
                SqlValue::Text(stored),
                SqlValue::Text(refused),
            ] => Ok(OutcomeDivergence {
                transaction: txn.as_slice().try_into().map_err(|_| corrupt())?,
                publisher: PublicKey::from_bytes(publisher.as_slice().try_into().map_err(|_| corrupt())?)
                    .map_err(|_| corrupt())?,
                stored: stored.clone(),
                refused: refused.clone(),
            }),
            _ => Err(corrupt()),
        })
        .collect()
    }

    pub async fn publish_missing_outcomes(&self) -> Result<(), Error> {
        let mut inner = self.inner.lock(Class::Background).await;
        let time = now()?;
        transaction!(inner, |inner| {
            inner.publish_missing_outcomes(time).await
        })
    }

    pub async fn awaiting_outcomes(&self) -> Result<Vec<Commitment>, Error> {
        let inner = self.inner.lock(Class::Background).await;
        collect_rows(
            inner
                .conn
                .query(
                    "SELECT DISTINCT r.txn FROM secure_replayed r JOIN secure_applied a ON a.hash=r.hash JOIN secure_seal_bounds b ON b.scope=r.scope AND a.position<=b.position WHERE r.sealed=0 ORDER BY r.txn",
                    (),
                )
                .await?,
        )
        .await?
        .into_iter()
        .map(|row| match row.as_slice() {
            [SqlValue::Blob(txn)] => txn
                .as_slice()
                .try_into()
                .map_err(|_| Error::Corrupt("replayed transaction".into())),
            _ => Err(Error::Corrupt("replayed transaction".into())),
        })
        .collect()
    }
}

impl<A: RowApplication> Inner<A> {
    async fn publish_missing_outcomes(&self, time: u64) -> Result<(), Error> {
        let corrupt = || Error::Corrupt("replayed member".into());
        let open = collect_rows(
            self.conn
                .query(
                    "SELECT txn FROM secure_replayed r WHERE sealed=0 AND members>1 GROUP BY txn HAVING COUNT(*)=MAX(members) AND (SELECT COUNT(*) FROM secure_outcome_members m WHERE m.txn=r.txn)<MAX(members)",
                    (),
                )
                .await?,
        )
        .await?;
        for row in open {
            let [SqlValue::Blob(txn)] = row.as_slice() else {
                return Err(corrupt());
            };
            let transaction: Commitment = txn.as_slice().try_into().map_err(|_| corrupt())?;
            let mut frames = Vec::new();
            let mut dropped = None;
            for member in collect_rows(
                self.conn
                    .query(
                        "SELECT hash,member,dropped FROM secure_replayed WHERE txn=? ORDER BY member",
                        [txn.clone()],
                    )
                    .await?,
            )
            .await?
            {
                let [SqlValue::Blob(hash), SqlValue::Integer(index), reason] = member.as_slice()
                else {
                    return Err(corrupt());
                };
                let hash = Hash::try_from(hash.as_slice()).map_err(|_| corrupt())?;
                let frame = self.frame(hash).await?.ok_or_else(corrupt)?;
                if let SqlValue::Text(reason) = reason {
                    dropped = Some(reason_named(reason)?);
                }
                frames.push((frame, *index as u16));
            }
            let held: Vec<_> = frames
                .iter()
                .map(|(frame, index)| (frame, *index))
                .collect();
            self.publish_outcome(transaction, &held, dropped, time)
                .await?;
        }
        Ok(())
    }
}
