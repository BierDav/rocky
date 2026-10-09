//! The horizon signal, checkpoint transfer and rebootstrap.
use super::*;

/// Logs of a scope whose operations up to a sequence were pruned.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HorizonPage {
    pub scope: ResourceScope,
    pub logs: Vec<(PublicKey, u64)>,
    pub next: Option<PublicKey>,
}
/// Checkpoint rows in row order, encrypted with the scope key.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckpointPage {
    pub scope: ResourceScope,
    pub checkpoint: Commitment,
    pub after: Option<Commitment>,
    pub rows: EncryptedPackage,
    pub next: Option<Commitment>,
}
/// Row, stored record and the clock of the row's creation operation.
type PageRow = (Commitment, Vec<u8>, u64);
#[derive(Serialize, Deserialize)]
struct PagePlain {
    checkpoint: Commitment,
    after: Option<Commitment>,
    next: Option<Commitment>,
    rows: Vec<PageRow>,
    strong: Option<crate::command::history::StrongState>,
}
/// What a rebootstrap did with the operations this replica held.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RebootstrapReport {
    pub checkpoint: Commitment,
    /// Held operations the checkpoint covers; their copies were dropped.
    pub superseded: u64,
    /// Operations beyond the checkpoint that were applied again on top of it.
    pub reapplied: Vec<Hash>,
    /// Operations beyond the checkpoint that no longer apply, with the reason.
    pub rejected: Vec<(Hash, String)>,
    /// Operations beyond the checkpoint that wait for a dependency or key.
    pub pending: Vec<Hash>,
}
pub(super) struct Staging {
    pub(super) signed: SignedCheckpoint,
    cursor: Option<Commitment>,
    complete: bool,
    pub(super) sets: Sets,
}

impl<A: RowApplication> Inner<A> {
    /// Custody or read authority of a peer over a whole scope.
    pub(crate) fn authorize_peer(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scope: ResourceScope,
    ) -> Result<(), Error> {
        // A partition not learned here has nothing to disclose, and the peer's
        // grant needs its tuple. Frames the peer carries are admitted by their
        // authors' grants; refusing would end the session that brings it.
        if self.partitioned(scope) && !self.partitions.contains_key(&scope) {
            self.verify_chain(credentials, peer, now()?)?;
            return Ok(());
        }
        let grants = self.verify_chain_scopes(credentials, peer, now()?, &[scope])?;
        if !grants.iter().any(|grant| {
            grant.scope == scope
                && grant.rows == RowSelection::All
                && (grant.rights.contains(&Right::Custody) || grant.rights.contains(&Right::Read))
        }) {
            return Err(denied("ciphertext custody denied"));
        }
        Ok(())
    }
    pub(super) async fn staging(&self, id: i64) -> Result<Option<Staging>, Error> {
        let mut result = self
            .conn
            .query(
                "SELECT signed,cursor,complete,sets FROM secure_rebootstrap WHERE scope=?",
                [id],
            )
            .await?;
        let Some(row) = result.next().await? else {
            return Ok(None);
        };
        Ok(Some(Staging {
            signed: SignedCheckpoint::from_bytes(&row.get::<Vec<u8>>(0)?)?,
            cursor: match row.get_value(1)? {
                crate::sqlite::Value::Blob(bytes) => Some(row_of(&bytes)?),
                _ => None,
            },
            complete: row.get::<i64>(2)? == 1,
            sets: Sets::from_bytes(&row.get::<Vec<u8>>(3)?)?,
        }))
    }
    async fn clear_staging(&self, id: i64) -> Result<(), Error> {
        for table in [
            "secure_rebootstrap",
            "secure_rebootstrap_rows",
            "secure_rebootstrap_stubs",
            "secure_rebootstrap_strong",
        ] {
            self.conn
                .execute(format!("DELETE FROM {table} WHERE scope=?"), [id])
                .await?;
        }
        Ok(())
    }
    /// Whether the scope has pruned history a peer may be behind.
    pub(crate) async fn has_horizon(&self, scope: ResourceScope) -> Result<bool, Error> {
        match self.find_scope_id(scope).await? {
            Some(id) => Ok(!self.logs(id, None, true, 1).await?.is_empty()),
            None => Ok(false),
        }
    }
    /// The checkpoint this replica can serve: verified and signed under the
    /// current policy.
    pub(super) async fn served_checkpoint(
        &mut self,
        scope: ResourceScope,
    ) -> Result<Option<(i64, SignedCheckpoint)>, Error> {
        self.settle(scope).await?;
        let Some(id) = self.find_scope_id(scope).await? else {
            return Ok(None);
        };
        let Some(record) = self.checkpoint_record(id, true).await? else {
            return Ok(None);
        };
        let signed = self.verify_record(scope, &record)?;
        if signed.statement.horizon != self.mergeable_horizon(scope).await? {
            return Ok(None);
        }
        if signed.statement.strong.base.map_or(0, |base| base.0)
            < self.strong_base(scope).await?.sequence()
        {
            return Ok(None);
        }
        // A checkpoint of a generation behind the horizon is offered again
        // once maintenance seals one of the current generation.
        if self.admits(signed.statement.definition).is_err() {
            return Ok(None);
        }
        signed.verify_authority(self, now()?)?;
        // Pages are sealed under the current key; a checkpoint of an earlier
        // key epoch is served again once maintenance seals the next one.
        if self.scope_key(scope)?.0 != signed.statement.key_epoch {
            return Ok(None);
        }
        Ok(Some((id, signed)))
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// The pruned prefix of every log in a scope, 256 logs per page. A peer
    /// compares it with what it holds through `behind_horizon`.
    pub async fn horizon_page(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scope: ResourceScope,
        after: Option<PublicKey>,
    ) -> Result<HorizonPage, Error> {
        let mut inner = self.inner.lock().await;
        inner.flush()?;
        inner.authorize_peer(peer, credentials, scope)?;
        let mut logs = match inner.find_scope_id(scope).await? {
            Some(id) => inner.logs(id, after, true, STEP_ROWS + 1).await?,
            None => vec![],
        };
        let more = logs.len() > STEP_ROWS;
        logs.truncate(STEP_ROWS);
        Ok(HorizonPage {
            scope,
            next: logs.last().filter(|_| more).map(|(log, _)| log.author),
            logs: logs
                .into_iter()
                .map(|(log, horizon)| (log.author, horizon as u64))
                .collect(),
        })
    }
    /// True when a peer pruned operations this replica neither holds nor has
    /// pruned itself: the gap can only be closed by a rebootstrap.
    pub async fn behind_horizon(&self, horizon: &HorizonPage) -> Result<bool, Error> {
        let inner = self.inner.lock().await;
        let id = inner.find_scope_id(horizon.scope).await?;
        for (author, sequence) in &horizon.logs {
            let own = match id {
                Some(id) => inner.log(id, *author).await?.map_or(-1, |log| log.horizon),
                None => -1,
            };
            let needed = *sequence as i64 - own;
            if needed <= 0 {
                continue;
            }
            let held = collect_rows(
                inner
                    .conn
                    .query(
                        "SELECT COUNT(*) FROM secure_operations WHERE author=? AND scope=? AND sequence>? AND sequence<=?",
                        bindings(vec![
                            SqlValue::Blob(author.as_bytes().to_vec()),
                            SqlValue::Blob(encode(&horizon.scope)?),
                            SqlValue::Integer(own),
                            SqlValue::Integer(*sequence as i64),
                        ]),
                    )
                    .await?,
            )
            .await?;
            if held != vec![vec![SqlValue::Integer(needed)]] {
                return Ok(true);
            }
        }
        Ok(false)
    }
    /// The signed checkpoint a peer behind the horizon starts from.
    pub async fn checkpoint_offer(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scope: ResourceScope,
    ) -> Result<Option<SignedCheckpoint>, Error> {
        let may_sign = {
            let mut inner = self.inner.lock().await;
            inner.authorize_peer(peer, credentials, scope)?;
            // A partitioned table's own scope holds no rows; each partition
            // scope has its checkpoint.
            if inner.partitioned(scope) && !inner.partitions.contains_key(&scope) {
                return Ok(None);
            }
            // A keyless custody replica holds only ciphertext: no offer.
            if !inner.can_read_key(scope, &inner.local_grants(now()?)?) {
                return Ok(None);
            }
            let (key_epoch, _) = inner.scope_key(scope)?;
            inner.settle(scope).await?;
            // A bootstrap requester refuses its own checkpoint signature.
            if let Some(id) = inner.find_scope_id(scope).await?
                && let Some(signed) = inner
                    .checkpoint_record(id, true)
                    .await?
                    .and_then(|record| record.signed)
                && signed.signer() != peer
                && signed.statement.key_epoch == key_epoch
                && signed.statement.horizon == inner.mergeable_horizon(scope).await?
                && signed.statement.strong.head == inner.capture_strong(scope).await?.head
                && signed.statement.strong.base.map_or(0, |base| base.0)
                    >= inner.strong_base(scope).await?.sequence()
                && signed.verify_authority(&inner, now()?).is_ok()
            {
                // Keep a transferable cut stable across page/session retries.
                inner.flush()?;
                return Ok(inner
                    .served_checkpoint(scope)
                    .await?
                    .filter(|(_, signed)| signed.signer() != peer)
                    .map(|(_, signed)| signed));
            }
            inner.local_grants(now()?)?.iter().any(|grant| {
                grant.scope == scope
                    && grant.rows == RowSelection::All
                    && [Right::Insert, Right::Update, Right::Delete]
                        .iter()
                        .any(|right| grant.rights.contains(right))
            })
        };
        if may_sign {
            self.checkpoint(scope).await?;
        }
        let mut inner = self.inner.lock().await;
        inner.authorize_peer(peer, credentials, scope)?;
        inner.flush()?;
        Ok(inner
            .served_checkpoint(scope)
            .await?
            .filter(|(_, signed)| signed.signer() != peer)
            .map(|(_, signed)| signed))
    }
    /// One page of the checkpoint's rows after `after`, sealed with the scope
    /// key, within `byte_quota` encoded bytes. A checkpoint that was replaced
    /// meanwhile is refused; the peer starts again from a new offer.
    pub async fn checkpoint_page(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scope: ResourceScope,
        checkpoint: Commitment,
        after: Option<Commitment>,
        byte_quota: usize,
    ) -> Result<CheckpointPage, Error> {
        let mut inner = self.inner.lock().await;
        inner.flush()?;
        inner.authorize_peer(peer, credentials, scope)?;
        if !(4096..=MAX_SECURE_FRAME).contains(&byte_quota) {
            return Err(denied("sync quota"));
        }
        let (generation, key) = inner.scope_key(scope)?;
        let Some((id, signed)) = inner.served_checkpoint(scope).await? else {
            return Err(denied("no checkpoint to transfer"));
        };
        if signed.id()? != checkpoint {
            return Err(denied("checkpoint superseded; restart the rebootstrap"));
        }
        // Byte strings encode as integer arrays: at most four encoded bytes per stored byte.
        let strong = if after.is_none() {
            let mut record = inner
                .conn
                .query(
                    "SELECT state FROM secure_checkpoint_strong WHERE scope=?",
                    [id],
                )
                .await?;
            let mut state: crate::command::history::StrongState = decode(
                &record
                    .next()
                    .await?
                    .ok_or_else(|| denied("checkpoint strong state missing"))?
                    .get::<Vec<u8>>(0)?,
            )?;
            if let Some(result) = &mut state.latest {
                result.parts.retain(|part| part.scope == scope);
            }
            Some(state)
        } else {
            None
        };
        let strong_bytes = encode(&strong)?.len();
        let budget = (byte_quota - 2048) / 4;
        if strong_bytes > budget {
            return Err(denied("byte quota smaller than checkpoint strong state"));
        }
        let budget = budget - strong_bytes;
        let records = collect_rows(
            inner
                .conn
                .query(
                    "SELECT row,fields FROM secure_checkpoint_rows WHERE scope=? AND row>? ORDER BY row LIMIT ?",
                    bindings(vec![
                        SqlValue::Integer(id),
                        SqlValue::Blob(after.map(|row| row.to_vec()).unwrap_or_default()),
                        SqlValue::Integer(STEP_ROWS as i64 + 1),
                    ]),
                )
                .await?,
        )
        .await?;
        let mut plain = PagePlain {
            checkpoint,
            after,
            next: None,
            rows: vec![],
            strong,
        };
        let mut used = 0;
        for record in &records {
            let [SqlValue::Blob(row), SqlValue::Blob(fields)] = record.as_slice() else {
                return Err(Error::Corrupt("checkpoint row".into()));
            };
            let weight = 48 + fields.len();
            if plain.rows.len() == STEP_ROWS || used + weight > budget {
                plain.next = plain.rows.last().map(|(row, _, _)| *row);
                break;
            }
            used += weight;
            let creation = decode::<Materialized>(fields)?.creation;
            let (_, clock) = inner
                .stub(creation)
                .await?
                .ok_or_else(|| Error::Corrupt("checkpoint row creation is unknown".into()))?;
            plain.rows.push((row_of(row)?, fields.clone(), clock));
        }
        if plain.rows.is_empty() && !records.is_empty() {
            return Err(denied("byte quota smaller than one checkpoint row"));
        }
        let page = CheckpointPage {
            scope,
            checkpoint,
            after,
            next: plain.next,
            rows: EncryptedPackage::seal(
                scope,
                generation,
                PackageKind::Checkpoint,
                &key,
                &encode(&plain)?,
            )?,
        };
        if encode(&page)?.len() > byte_quota {
            return Err(denied("checkpoint page exceeds the byte quota"));
        }
        Ok(page)
    }
    /// Starts importing a peer's signed checkpoint for a scope this replica
    /// reads in whole. Nothing changes until `finish_rebootstrap`.
    pub async fn begin_rebootstrap(&self, signed: SignedCheckpoint) -> Result<(), Error> {
        let mut inner = self.inner.lock().await;
        signed.verify_authority(&inner, now()?)?;
        let statement = &signed.statement;
        let scope = statement.scope;
        let generation = inner.generation_of(statement.definition, true)?;
        if statement.schema != generation.schema
            || statement.handler != generation.handler
            || signed.signer() == inner.config.node.public_key()
            || inner.scope_key(scope)?.0 != statement.key_epoch
        {
            return Err(denied(
                "checkpoint definition/generation/key epoch mismatch",
            ));
        }
        let bytes = signed.to_bytes()?;
        transaction!(inner, |inner| {
            let id = inner.scope_id(scope).await?;
            inner.check_frontier(id, &signed).await?;
            if let Some(finalized) = &signed.statement.finalized {
                for descriptor in &finalized.statement.bases {
                    let expected = super::super::existence::ExistenceBaseAttachment {
                        horizon: finalized.clone(),
                        scope: descriptor.scope,
                    };
                    if !inner
                        .existence_base(descriptor.scope)
                        .await?
                        .map(|base| base.covers(&expected))
                        .transpose()?
                        .unwrap_or(false)
                    {
                        return Err(denied(
                            "import the authenticated existence attachment before switching checkpoint rows",
                        ));
                    }
                }
            }
            inner.clear_staging(id).await?;
            inner
                .conn
                .execute(
                    "INSERT INTO secure_rebootstrap VALUES(?,?,NULL,0,?)",
                    bindings(vec![
                        SqlValue::Integer(id),
                        SqlValue::Blob(bytes.clone()),
                        SqlValue::Blob(Sets::new().to_bytes()),
                    ]),
                )
                .await?;
            Ok(())
        })
    }
    /// The checkpoint being imported for a scope, the last row staged and
    /// whether every page arrived.
    pub async fn rebootstrap_progress(
        &self,
        scope: ResourceScope,
    ) -> Result<Option<(Commitment, Option<Commitment>, bool)>, Error> {
        let inner = self.inner.lock().await;
        let Some(id) = inner.find_scope_id(scope).await? else {
            return Ok(None);
        };
        match inner.staging(id).await? {
            Some(staging) => Ok(Some((
                staging.signed.id()?,
                staging.cursor,
                staging.complete,
            ))),
            None => Ok(None),
        }
    }
    /// Stages the next page of checkpoint rows; pages arrive in order.
    pub async fn stage_checkpoint_page(&self, page: CheckpointPage) -> Result<(), Error> {
        let mut inner = self.inner.lock().await;
        let scope = page.scope;
        let (_, key) = inner.scope_key(scope)?;
        let id = inner
            .find_scope_id(scope)
            .await?
            .ok_or_else(|| denied("no rebootstrap in progress"))?;
        let staging = inner
            .staging(id)
            .await?
            .ok_or_else(|| denied("no rebootstrap in progress"))?;
        if staging.signed.id()? != page.checkpoint
            || staging.complete
            || staging.cursor != page.after
            || page.rows.scope != scope
            || page.rows.kind != PackageKind::Checkpoint
            || page.rows.generation != staging.signed.statement.key_epoch
        {
            return Err(denied("unexpected checkpoint page"));
        }
        let plain: PagePlain = decode(&page.rows.open(&key)?)?;
        if plain.checkpoint != page.checkpoint
            || plain.after != page.after
            || plain.next != page.next
            || plain.rows.is_empty() && plain.next.is_some()
            || plain.rows.len() > STEP_ROWS
        {
            return Err(denied("checkpoint page binding mismatch"));
        }
        transaction!(inner, |inner| {
            match (&plain.strong, page.after) {
                (Some(state), None) => {
                    state.binds(&staging.signed)?;
                    inner
                        .conn
                        .execute(
                            "INSERT INTO secure_rebootstrap_strong VALUES(?,?)",
                            bindings(vec![SqlValue::Integer(id), SqlValue::Blob(encode(state)?)]),
                        )
                        .await?;
                }
                (None, Some(_)) => {}
                _ => return Err(denied("checkpoint strong page binding mismatch")),
            }
            let mut sets = staging.sets.clone();
            let mut cursor = staging.cursor;
            for (row, fields, clock) in &plain.rows {
                if cursor.is_some_and(|cursor| *row <= cursor) {
                    return Err(denied("checkpoint rows out of order"));
                }
                cursor = Some(*row);
                sets.replace(row, None, fields)?;
                let record = decode::<Materialized>(fields)?;
                // A row moved out leaves a tombstone without an image.
                if let Some(tuple) = &staging.signed.statement.partition
                    && !(record.deleted && record.fields.is_empty())
                {
                    let change = RowChange {
                        scope: staging.signed.statement.scope,
                        row: record
                            .key
                            .ok_or_else(|| denied("partition checkpoint row lacks key"))?,
                        before: None,
                        after: Some(record.fields.clone()),
                    };
                    if inner
                        .app
                        .row_partition(&change)
                        .map_err(|error| denied(&error))?
                        .as_ref()
                        != Some(tuple)
                    {
                        return Err(denied("checkpoint row differs from its partition"));
                    }
                }

                if let Some(identity) = record.identity {
                    let identity = rocky_core::RowIdentity::from_integer(identity)
                        .map_err(|error| denied(&error.to_string()))?;
                    if !staging.signed.statement.identities.iter().any(|mark| {
                        mark.author == identity.author() && mark.sequence >= identity.sequence()
                    }) {
                        return Err(denied("checkpoint identity exceeds its permanent mark"));
                    }
                }
                let creation = record.creation;
                inner
                    .conn
                    .execute(
                        "INSERT INTO secure_rebootstrap_rows VALUES(?,?,?)",
                        bindings(vec![
                            SqlValue::Integer(id),
                            SqlValue::Blob(row.to_vec()),
                            SqlValue::Blob(fields.clone()),
                        ]),
                    )
                    .await?;
                let known = collect_rows(
                    inner
                        .conn
                        .query(
                            "SELECT clock FROM secure_rebootstrap_stubs WHERE scope=? AND hash=?",
                            bindings(vec![
                                SqlValue::Integer(id),
                                SqlValue::Blob(creation.as_bytes().to_vec()),
                            ]),
                        )
                        .await?,
                )
                .await?;
                match known.first().map(Vec::as_slice) {
                    Some([SqlValue::Integer(known)]) if *known as u64 == *clock => {}
                    Some(_) => return Err(denied("checkpoint creation clocks disagree")),
                    None => {
                        sets.create(creation, *clock);
                        inner
                            .conn
                            .execute(
                                "INSERT INTO secure_rebootstrap_stubs VALUES(?,?,?)",
                                bindings(vec![
                                    SqlValue::Integer(id),
                                    SqlValue::Blob(creation.as_bytes().to_vec()),
                                    SqlValue::Integer(*clock as i64),
                                ]),
                            )
                            .await?;
                    }
                }
            }
            inner
                .conn
                .execute(
                    "UPDATE secure_rebootstrap SET cursor=?,complete=?,sets=? WHERE scope=?",
                    bindings(vec![
                        cursor.map_or(SqlValue::Null, |row| SqlValue::Blob(row.to_vec())),
                        SqlValue::Integer(i64::from(plain.next.is_none())),
                        SqlValue::Blob(sets.to_bytes()),
                        SqlValue::Integer(id),
                    ]),
                )
                .await?;
            inner.fault(Stage::Import)?;
            Ok(())
        })
    }
    /// Verifies the staged rows against the signed checkpoint, then switches
    /// the scope to them in one transaction: held operations the checkpoint
    /// covers are dropped, and every other operation this replica had applied
    /// is applied again on top or reported as rejected with its reason.
    pub async fn finish_rebootstrap(
        &self,
        scope: ResourceScope,
    ) -> Result<RebootstrapReport, Error> {
        let mut inner = self.inner.lock().await;
        let time = now()?;
        let (generation, key) = inner.scope_key(scope)?;
        let id = inner
            .find_scope_id(scope)
            .await?
            .ok_or_else(|| denied("no rebootstrap in progress"))?;
        let staging = inner
            .staging(id)
            .await?
            .ok_or_else(|| denied("no rebootstrap in progress"))?;
        if !staging.complete {
            return Err(denied("checkpoint rows are incomplete"));
        }
        let signed = staging.signed;
        signed.verify_authority(&inner, time)?;
        let statement = signed.statement.clone();
        if inner.rebootstrap_moves(id).await? != statement.moves
            || staging.sets.rows_summary() != statement.rows
            || staging.sets.tombstones != statement.tombstones
            || staging.sets.creations_summary() != statement.creations
            || generation != statement.key_epoch
        {
            // Unverifiable rows are discarded; the scope is untouched.
            transaction!(inner, |inner| inner.clear_staging(id).await)?;
            return Err(Error::Corrupt(
                "checkpoint rows differ from the signed checkpoint".into(),
            ));
        }
        let sets = staging.sets;
        let report = transaction!(inner, |inner| {
            inner.check_frontier(id, &signed).await?;
            if let Some(horizon) = &signed.statement.horizon {
                inner.adopt_checkpoint_horizon(horizon).await?;
                inner
                    .index_pending_horizon(
                        horizon,
                        signed.statement.finalized.as_ref() != Some(horizon),
                    )
                    .await?;
            }
            let mut report = RebootstrapReport {
                checkpoint: signed.id()?,
                superseded: 0,
                reapplied: vec![],
                rejected: vec![],
                pending: vec![],
            };
            let reapply = inner.supersede(scope, &statement, &mut report).await?;
            let blob = encode(&scope)?;
            inner
                .conn
                .execute("DELETE FROM secure_rows WHERE scope=?", [blob.clone()])
                .await?;
            inner
                .conn
                .execute(
                    "INSERT INTO secure_rows SELECT ?,row,fields,X'' FROM secure_rebootstrap_rows WHERE scope=?",
                    bindings(vec![SqlValue::Blob(blob), SqlValue::Integer(id)]),
                )
                .await?;
            let permanent = inner.identity_marks(scope).await?;
            inner
                .conn
                .execute(
                    "DELETE FROM secure_identity_marks WHERE scope=?",
                    [encode(&scope)?],
                )
                .await?;
            for mark in &statement.identities {
                inner.put_identity_mark(scope, mark).await?;
            }
            inner.reproject(scope).await?;
            for sql in [
                "DELETE FROM secure_checkpoint_rows WHERE scope=?",
                "INSERT INTO secure_checkpoint_rows SELECT scope,row,fields FROM secure_rebootstrap_rows WHERE scope=?",
                "DELETE FROM secure_checkpoint_overlay WHERE scope=?",
                "DELETE FROM secure_checkpoint_scratch WHERE scope=?",
                "DELETE FROM secure_stubs WHERE scope=?",
                "INSERT INTO secure_stubs SELECT hash,scope,clock,1 FROM secure_rebootstrap_stubs WHERE scope=?",
                "DELETE FROM secure_logs WHERE scope=?",
                "DELETE FROM secure_checkpoints WHERE scope=?",
            ] {
                inner.conn.execute(sql, [id]).await?;
            }
            inner
                .conn
                .execute("DELETE FROM secure_checkpoint_stubs WHERE scope=?", [id])
                .await?;
            inner.conn.execute("INSERT INTO secure_checkpoint_stubs SELECT * FROM secure_rebootstrap_stubs WHERE scope=?", [id]).await?;
            inner.restore_protection_stubs(scope, id).await?;
            for log in &statement.frontier {
                inner
                    .conn
                    .execute(
                        "INSERT INTO secure_logs VALUES(?,?,?,?,?,?)",
                        bindings(vec![
                            SqlValue::Integer(id),
                            SqlValue::Blob(log.author.as_bytes().to_vec()),
                            SqlValue::Integer(log.sequence as i64),
                            SqlValue::Blob(log.hash.as_bytes().to_vec()),
                            SqlValue::Integer(log.clock as i64),
                            SqlValue::Integer(log.sequence as i64),
                        ]),
                    )
                    .await?;
            }
            inner
                .store_scope_state(
                    id,
                    sets.rows,
                    sets.tombstones,
                    &sets.digest,
                    statement.operations,
                )
                .await?;
            // Operations applied from here on get later positions.
            inner
                .conn
                .execute(
                    "INSERT INTO secure_checkpoints SELECT ?,?,'verified',COALESCE(MAX(seq),0),COALESCE(MAX(seq),0),?,NULL,? FROM sqlite_sequence WHERE name='secure_applied'",
                    bindings(vec![
                        SqlValue::Integer(id),
                        SqlValue::Integer(statement.sequence as i64),
                        SqlValue::Blob(sets.to_bytes()),
                        SqlValue::Blob(signed.to_bytes()?),
                    ]),
                )
                .await?;
            inner
                .conn
                .execute(
                    "INSERT INTO secure_keys VALUES(?,?,?) ON CONFLICT(scope) DO NOTHING",
                    bindings(vec![
                        SqlValue::Blob(encode(&scope)?),
                        SqlValue::Integer(generation as i64),
                        SqlValue::Blob(rocky_core::commitment("rocky.scope-key.v1", &key).to_vec()),
                    ]),
                )
                .await?;
            for hash in reapply {
                let frame = inner
                    .frame(hash)
                    .await?
                    .ok_or_else(|| Error::Corrupt("retained operation".into()))?;
                match inner.apply_frame(&frame, time).await? {
                    incoming::Outcome::Applied => match inner.consumed(hash).await? {
                        Some(reason) => report.rejected.push((hash, reason)),
                        None => report.reapplied.push(hash),
                    },
                    incoming::Outcome::Rejected(reason) => report.rejected.push((hash, reason)),
                    incoming::Outcome::Waiting => report.pending.push(hash),
                }
            }
            for mark in &permanent {
                inner.put_identity_mark(scope, mark).await?;
            }
            let mut strong = inner
                .conn
                .query(
                    "SELECT state FROM secure_rebootstrap_strong WHERE scope=?",
                    [id],
                )
                .await?;
            let mut state: crate::command::history::StrongState = decode(
                &strong
                    .next()
                    .await?
                    .ok_or_else(|| denied("checkpoint strong state missing"))?
                    .get::<Vec<u8>>(0)?,
            )?;
            state.binds(&signed)?;
            state.checkpoint = Some(signed.clone());
            inner.install_strong(scope, &state).await?;
            inner.conn.execute("INSERT INTO secure_checkpoint_strong VALUES(?,?) ON CONFLICT(scope) DO UPDATE SET state=excluded.state", bindings(vec![SqlValue::Integer(id),SqlValue::Blob(encode(&state)?)])).await?;
            inner
                .conn
                .execute(
                    "DELETE FROM secure_horizon_bootstrap WHERE scope=?",
                    [encode(&scope)?],
                )
                .await?;
            inner.clear_staging(id).await?;
            inner
                .conn
                .execute(
                    "UPDATE secure_metadata SET revision=revision+1 WHERE id=1",
                    (),
                )
                .await?;
            inner.fault(Stage::Switch)?;
            Ok(report)
        })?;
        inner.verified_at.remove(&id);
        inner.publish_revision().await?;
        inner.process_pending(time).await?;
        Ok(report)
    }
}

impl<A: RowApplication> Inner<A> {
    /// A checkpoint that ends before this replica's own horizon would lose
    /// the operations between them.
    async fn check_frontier(&self, id: i64, signed: &SignedCheckpoint) -> Result<(), Error> {
        self.check_checkpoint_horizon(signed).await?;
        let base = self.strong_base(signed.statement.scope).await?;
        if base.sequence() > signed.statement.strong.base.map_or(0, |base| base.0) {
            return Err(denied("checkpoint behind strong horizon"));
        }
        for (log, horizon) in self.logs(id, None, true, MAX_CHECKPOINT_LOGS + 1).await? {
            if !signed.statement.frontier.iter().any(|position| {
                position.author == log.author && position.sequence as i64 >= horizon
            }) {
                return Err(denied("checkpoint is behind this replica's horizon"));
            }
        }
        Ok(())
    }
    /// Drops held operations the checkpoint covers and returns, in their old
    /// application order, the applied ones beyond it, reset to pending.
    async fn supersede(
        &self,
        scope: ResourceScope,
        statement: &CheckpointStatement,
        report: &mut RebootstrapReport,
    ) -> Result<Vec<Hash>, Error> {
        let mut reapply = Vec::new();
        let mut cursor = Vec::new();
        let mut bytes = 0;
        loop {
            let records = collect_rows(self.conn.query(
                "SELECT o.hash,o.header,o.status,LENGTH(o.header)+LENGTH(o.body),(SELECT position FROM secure_applied a WHERE a.hash=o.hash) FROM secure_operations o WHERE o.scope=? AND o.hash>? ORDER BY o.hash LIMIT 32",
                [encode(&scope)?, cursor.clone()],
            ).await?).await?;
            if records.is_empty() {
                break;
            }
            for record in records {
                let [
                    SqlValue::Blob(hash),
                    SqlValue::Blob(header),
                    SqlValue::Text(status),
                    SqlValue::Integer(size),
                    position,
                ] = record.as_slice()
                else {
                    return Err(Error::Corrupt("retained operation".into()));
                };
                cursor = hash.clone();
                let header: p2panda_core::Header<Extensions> = decode(header)?;
                let covering = statement
                    .frontier
                    .iter()
                    .find(|log| log.author == header.public_key && header.seq_num <= log.sequence);
                if let Some(log) = covering {
                    if header.seq_num == log.sequence && log.hash.as_bytes().as_slice() != hash {
                        return Err(denied("checkpoint frontier forks a retained log"));
                    }
                    for sql in [
                        "DELETE FROM secure_operations WHERE hash=?",
                        "DELETE FROM secure_outbox WHERE hash=?",
                        "DELETE FROM secure_ack WHERE hash=?",
                        "DELETE FROM secure_applied WHERE hash=?",
                    ] {
                        self.conn.execute(sql, [hash.clone()]).await?;
                    }
                    bytes += *size;
                    report.superseded += 1;
                } else if status == "applied" {
                    let SqlValue::Integer(position) = position else {
                        return Err(Error::Corrupt("applied position".into()));
                    };
                    self.conn
                        .execute(
                            "UPDATE secure_operations SET status='pending' WHERE hash=?",
                            [hash.clone()],
                        )
                        .await?;
                    self.conn
                        .execute("DELETE FROM secure_applied WHERE hash=?", [hash.clone()])
                        .await?;
                    reapply.push((*position, hash_of(hash)?));
                }
            }
        }
        self.conn
            .execute(
                "UPDATE secure_retained SET records=records-?,bytes=bytes-?",
                [report.superseded as i64, bytes],
            )
            .await?;
        reapply.sort();
        Ok(reapply.into_iter().map(|(_, hash)| hash).collect())
    }
}
