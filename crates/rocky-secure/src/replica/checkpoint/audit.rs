//! Open-time verification of the retained suffix and the explicit full audit.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditReport {
    /// Retained operations of the scope whose signatures and bindings verified.
    pub operations: u64,
    /// Stored rows compared with the scope's digest; zero without the key.
    pub rows: u64,
    /// The verified checkpoint the stored rows were checked against.
    pub checkpoint: Option<Commitment>,
    /// Applied operations after it that were replayed.
    pub replayed: u64,
}

fn mismatch() -> Error {
    denied("projection differs from signed encrypted history")
}

impl<A: RowApplication> Inner<A> {
    /// Streams the stored rows of one table and scope into their digest.
    /// With `stubs`, every row's creation operation must still be known.
    async fn row_sets(
        &self,
        sql: &str,
        scope: SqlValue,
        stubs: Option<i64>,
    ) -> Result<Sets, Error> {
        let mut sets = Sets::new();
        let mut cursor = Vec::new();
        loop {
            let chunk = collect_rows(
                self.conn
                    .query(
                        sql,
                        bindings(vec![
                            scope.clone(),
                            SqlValue::Blob(cursor.clone()),
                            SqlValue::Integer(STEP_ROWS as i64),
                        ]),
                    )
                    .await?,
            )
            .await?;
            let exhausted = chunk.len() < STEP_ROWS;
            for record in chunk {
                let [SqlValue::Blob(row), SqlValue::Blob(fields)] = record.as_slice() else {
                    return Err(Error::Corrupt("stored row".into()));
                };
                sets.replace(&row_of(row)?, None, fields)?;
                if let Some(id) = stubs {
                    let creation = decode::<Materialized>(fields)?.creation;
                    if !self.retains(creation).await?
                        && self.stub(creation).await?.is_none_or(|stub| stub.0 != id)
                    {
                        return Err(Error::Corrupt(
                            "a stored row's creation operation is unknown".into(),
                        ));
                    }
                }
                cursor = row.clone();
            }
            if exhausted {
                return Ok(sets);
            }
        }
    }
    /// Verifies every keyed scope's operations after its checkpoint. History
    /// a verified checkpoint covers is left to `audit`; the key records are
    /// checked by `adopt_keys` before.
    pub(in crate::replica) async fn verify_open(&mut self) -> Result<(), Error> {
        let scopes: Vec<_> = self.config.keys.iter().map(|key| key.scope).collect();
        for scope in scopes {
            self.settle(scope).await?;
            self.verify_tail(scope).await?;
        }
        Ok(())
    }
    /// Replays the applied operations after the scope's verified checkpoint
    /// over it and compares the result with the stored rows. Returns the
    /// checkpoint and the operations replayed.
    async fn verify_tail(
        &mut self,
        scope: ResourceScope,
    ) -> Result<(Option<SignedCheckpoint>, u64), Error> {
        let Some(id) = self.find_scope_id(scope).await? else {
            return Ok((None, 0));
        };
        let record = self.checkpoint_record(id, true).await?;
        let (signed, mut cursor, mut sets) = match record {
            Some(record) => (
                Some(self.verify_record(scope, &record)?),
                record.position,
                record.sets,
            ),
            // The pages a direct import stored so far are the base that
            // local writes applied over.
            None => match self.staging(id).await?.filter(|staging| staging.direct) {
                Some(staging) => (None, 0, staging.sets),
                None => (None, 0, Sets::new()),
            },
        };
        let clear = "DELETE FROM secure_checkpoint_scratch WHERE scope=?";
        transaction!(@commit self, |inner| {
            inner.conn.execute(clear, [id]).await?;
            Ok(())
        })?;
        let replayed = match collect_rows(
            self.conn
                .query(
                    "SELECT COUNT(*) FROM secure_applied WHERE scope=? AND position>?",
                    [id, cursor],
                )
                .await?,
        )
        .await?
        .first()
        .map(Vec::as_slice)
        {
            Some([SqlValue::Integer(count)]) => *count as u64,
            _ => return Err(Error::Corrupt("applied positions".into())),
        };
        loop {
            let (last, next) = transaction!(@commit self, |inner| {
                let mut next = sets.clone();
                let last = inner
                    .replay(
                        scope,
                        id,
                        Overlay::Scratch,
                        (cursor, i64::MAX),
                        STEP_OPERATIONS,
                        &mut next,
                    )
                    .await?;
                Ok((last, next))
            })?;
            let Some(last) = last else {
                break;
            };
            cursor = last;
            sets = next;
        }
        let (rows, tombstones, digest, _) = self.scope_state(id).await?;
        let differing = collect_rows(
            self.conn
                .query(
                    "SELECT 1 FROM secure_checkpoint_scratch s LEFT JOIN secure_rows r ON r.scope=? AND r.row=s.row WHERE s.scope=? AND (r.fields IS NULL OR r.fields<>s.fields) LIMIT 1",
                    bindings(vec![SqlValue::Blob(encode(&scope)?), SqlValue::Integer(id)]),
                )
                .await?,
        )
        .await?;
        transaction!(@commit self, |inner| {
            inner.conn.execute(clear, [id]).await?;
            Ok(())
        })?;
        if !differing.is_empty()
            || sets.rows != rows
            || sets.tombstones != tombstones
            || sets.digest != digest
        {
            return Err(mismatch());
        }
        Ok((signed, replayed))
    }
    /// Verifies every retained operation of a scope in batches.
    async fn audit_operations(&self, scope: ResourceScope, id: Option<i64>) -> Result<u64, Error> {
        let mut cursor = Vec::new();
        let mut count = 0;
        let mut applied = 0;
        loop {
            let records = collect_rows(self.conn.query(
                "SELECT hash,header,body,status,EXISTS(SELECT 1 FROM secure_applied a WHERE a.hash=o.hash) FROM secure_operations o WHERE scope=? AND hash>? ORDER BY hash LIMIT 32",
                [encode(&scope)?, cursor.clone()],
            ).await?).await?;
            if records.is_empty() {
                break;
            }
            for record in records {
                let [
                    SqlValue::Blob(hash),
                    SqlValue::Blob(header),
                    SqlValue::Blob(body),
                    SqlValue::Text(status),
                    SqlValue::Integer(positioned),
                ] = record.as_slice()
                else {
                    return Err(denied("corrupt retained operation"));
                };
                cursor = hash.clone();
                count += 1;
                // Every retained operation has the chain its header names.
                let frame = self.stored_frame(header.clone(), body.clone()).await?;
                let operation = frame.operation()?;
                if operation.hash.as_bytes().as_slice() != hash
                    || self.known(operation.header.extensions.manifest).is_err()
                    || operation.header.extensions.scope != scope
                    || !["applied", "pending", "rejected"].contains(&status.as_str())
                    || (status == "applied") != (*positioned == 1)
                {
                    return Err(denied("corrupt retained history binding"));
                }
                if status == "applied" {
                    applied += 1;
                    if !self.dependencies_ready(&frame, &BTreeSet::new()).await? {
                        return Err(denied("applied encrypted dependency missing"));
                    }
                }
            }
        }
        let positions = match id {
            Some(id) => {
                collect_rows(
                    self.conn
                        .query("SELECT COUNT(*) FROM secure_applied WHERE scope=?", [id])
                        .await?,
                )
                .await?
            }
            None => vec![vec![SqlValue::Integer(0)]],
        };
        if positions != vec![vec![SqlValue::Integer(applied)]] {
            return Err(Error::Corrupt(
                "applied positions differ from retained operations".into(),
            ));
        }
        Ok(count)
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// The full verification `open` no longer performs: every retained
    /// operation of the scope, the retained-history counters and mutation
    /// handles and, with the scope key, the stored rows against their digest,
    /// the checkpoint rows against the signed checkpoint and a replay of the
    /// operations after it. Memory is bounded by one batch; the replica is
    /// locked for the duration.
    pub async fn audit(&self, scope: ResourceScope) -> Result<AuditReport, Error> {
        let mut inner = self.inner.lock().await;
        let id = inner.find_scope_id(scope).await?;
        let mut report = AuditReport {
            operations: inner.audit_operations(scope, id).await?,
            rows: 0,
            checkpoint: None,
            replayed: 0,
        };
        let counters = rows(
            &inner.conn,
            "SELECT (SELECT COUNT(*) FROM secure_operations)=records AND (SELECT COALESCE(SUM(LENGTH(header)+LENGTH(body)),0) FROM secure_operations)=bytes AND (SELECT COUNT(*) FROM secure_handles)=handles FROM secure_retained",
        )
        .await?;
        if counters != vec![vec![SqlValue::Integer(1)]] {
            return Err(Error::Corrupt("retained history counters".into()));
        }
        let mut handles = Lt::new();
        let mut cursor = Vec::new();
        loop {
            let chunk = collect_rows(
                inner
                    .conn
                    .query(
                        "SELECT handle,input,hash FROM secure_handles WHERE handle>? ORDER BY handle LIMIT 256",
                        [cursor.clone()],
                    )
                    .await?,
            )
            .await?;
            let Some(last) = chunk.last() else {
                break;
            };
            if let Some(SqlValue::Blob(handle)) = last.first() {
                cursor = handle.clone();
            }
            for record in &chunk {
                let [
                    SqlValue::Blob(handle),
                    SqlValue::Blob(input),
                    SqlValue::Blob(hash),
                ] = record.as_slice()
                else {
                    return Err(Error::Corrupt("mutation handle".into()));
                };
                handles.add(HANDLE_LABEL, &handle_item(handle, input, hash));
            }
        }
        if handles != inner.handle_set().await?.1 {
            return Err(Error::Corrupt("mutation handle digest".into()));
        }
        let (Some(id), Ok(_)) = (id, inner.scope_key(scope)) else {
            return Ok(report);
        };
        inner.settle(scope).await?;
        let live = inner
            .row_sets(
                "SELECT row,fields FROM secure_rows WHERE scope=? AND row>? ORDER BY row LIMIT ?",
                SqlValue::Blob(encode(&scope)?),
                Some(id),
            )
            .await?;
        let (rows, tombstones, digest, _) = inner.scope_state(id).await?;
        if live.rows != rows || live.tombstones != tombstones || live.digest != digest {
            return Err(mismatch());
        }
        report.rows = rows;
        if let Some(record) = inner.checkpoint_record(id, true).await? {
            let signed = inner.verify_record(scope, &record)?;
            let base = inner
                .row_sets(
                    "SELECT row,fields FROM secure_checkpoint_rows WHERE scope=? AND row>? ORDER BY row LIMIT ?",
                    SqlValue::Integer(id),
                    Some(id),
                )
                .await?;
            if base.rows_summary() != signed.statement.rows
                || base.tombstones != signed.statement.tombstones
            {
                return Err(Error::Corrupt(
                    "checkpoint rows differ from the signed checkpoint".into(),
                ));
            }
            for log in &signed.statement.frontier {
                if inner
                    .log(id, log.author)
                    .await?
                    .is_none_or(|local| local.sequence < log.sequence as i64)
                {
                    return Err(Error::Corrupt(
                        "a log is behind the checkpoint frontier".into(),
                    ));
                }
            }
        }
        let (signed, replayed) = inner.verify_tail(scope).await?;
        report.checkpoint = signed.map(|signed| signed.id()).transpose()?;
        report.replayed = replayed;
        Ok(report)
    }
}
