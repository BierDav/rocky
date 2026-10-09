use super::*;

impl<A: RowApplication> Inner<A> {
    /// Applies pending frames in bounded transactions, publishing after each
    /// commit. Applications before a failing one stay committed.
    pub(crate) async fn process_pending(&mut self, time: u64) -> Result<(), Error> {
        loop {
            let (more, failure) = transaction!(self, |inner| {
                match inner.apply_pending(time, group::GROUP_REQUESTS).await {
                    Ok(more) => Ok((more, None)),
                    Err(error) if retryable(&error) => Err(error),
                    Err(error) => Ok((false, Some(error))),
                }
            })?;
            self.publish_revision().await?;
            if let Some(error) = failure {
                return Err(error);
            }
            if !more {
                return Ok(());
            }
        }
    }
    /// Applies or rejects dependency-ready pending frames inside the caller's
    /// transaction, each application in its own savepoint. A failing
    /// application rolls back alone and ends the pass with its error. Returns
    /// true when `limit` stopped the pass before it settled.
    pub(super) async fn apply_pending(&mut self, time: u64, limit: usize) -> Result<bool, Error> {
        let mut changed = 0;
        loop {
            // Log order applies a chain that arrived in any order in one scan.
            let mut cursor = (-1i64, Vec::new());
            let mut advanced = false;
            loop {
                let records = collect_rows(self.conn.query(
                "SELECT hash,header,body,sequence FROM secure_operations WHERE status='pending' AND (sequence,hash)>(?,?) ORDER BY sequence,hash LIMIT 32",
                bindings(vec![SqlValue::Integer(cursor.0), SqlValue::Blob(cursor.1.clone())]),
            ).await?).await?;
                if records.is_empty() {
                    break;
                }
                for record in records {
                    if changed >= limit {
                        return Ok(true);
                    }
                    let [
                        SqlValue::Blob(hash),
                        SqlValue::Blob(header),
                        SqlValue::Blob(body),
                        SqlValue::Integer(sequence),
                    ] = record.as_slice()
                    else {
                        return Err(denied("corrupt pending operation"));
                    };
                    cursor = (*sequence, hash.clone());
                    let frame = EncryptedFrame {
                        header: header.clone(),
                        body: body.clone(),
                    };
                    if !matches!(
                        Box::pin(self.apply_frame(&frame, time)).await?,
                        Outcome::Waiting
                    ) {
                        changed += 1;
                        advanced = true;
                    }
                }
            }
            if !advanced {
                Box::pin(self.follow_moves(time)).await?;
                return Ok(false);
            }
        }
    }
    /// Applies or rejects one stored frame inside the caller's transaction,
    /// in its own savepoint. A storage failure rolls the savepoint back.
    pub(super) async fn apply_frame(
        &self,
        frame: &EncryptedFrame,
        time: u64,
    ) -> Result<Outcome, Error> {
        let operation = frame.operation()?;
        // A bundle applies its members together; a later scan finds them settled.
        match self.stored_status(operation.hash).await?.as_deref() {
            Some("applied") => return Ok(Outcome::Applied),
            Some("rejected") => return Ok(Outcome::Rejected("rejected earlier".into())),
            _ => {}
        }
        let grants = match self.verify_frame(frame, time) {
            Ok(grants) => grants,
            Err(error) => {
                self.reject(operation.hash).await?;
                return Ok(Outcome::Rejected(error.to_string()));
            }
        };
        let extension = &operation.header.extensions;
        if !self
            .horizon_allows_position(
                extension.scope,
                operation.header.public_key,
                operation.header.seq_num,
                false,
            )
            .await?
        {
            return Ok(Outcome::Waiting);
        }
        // An epoch this replica does not hold, later or earlier, waits for its key.
        let Some(key) = self.opening_key(extension.scope, extension.generation) else {
            return Ok(Outcome::Waiting);
        };
        // Current reader authorization is required even if a revoked key is still in memory.
        if !self.can_read_key(key.scope, &self.local_grants(time).unwrap_or_default()) {
            return Ok(Outcome::Waiting);
        }
        match self.dependencies_ready(frame, &BTreeSet::new()).await {
            Ok(true) => {}
            Ok(false) => return Ok(Outcome::Waiting),
            Err(error) => {
                self.reject(operation.hash).await?;
                return Ok(Outcome::Rejected(error.to_string()));
            }
        }
        let plaintext = decrypt(
            &key.key,
            &encode(&operation.header.extensions)?,
            &frame.body,
        );
        let Ok(plaintext) = plaintext else {
            return Ok(Outcome::Waiting);
        };
        let payload = RowPayload::decode(&plaintext);
        if let Ok(payload) = &payload
            && let Some(membership) = payload.membership()
        {
            return Box::pin(self.apply_bundle(&operation, membership, time)).await;
        }
        self.conn.execute("SAVEPOINT pending", ()).await?;
        let materialized = match payload {
            Ok(payload) => {
                self.materialize(frame, &payload.into_writes(), &grants)
                    .await
            }
            Err(error) => Err(error),
        };
        let result = match materialized {
            Ok(rows) => {
                async {
                    self.record_key(key).await?;
                    self.write_materialized(frame, rows).await?;
                    Ok(Outcome::Applied)
                }
                .await
            }
            Err(error) => self
                .reject(operation.hash)
                .await
                .map(|()| Outcome::Rejected(error.to_string())),
        };
        if result.is_err() {
            self.conn.execute("ROLLBACK TO pending", ()).await?;
        }
        self.conn.execute("RELEASE pending", ()).await?;
        result
    }
    pub(super) async fn stored_status(&self, hash: Hash) -> Result<Option<String>, Error> {
        let mut result = self
            .conn
            .query(
                "SELECT status FROM secure_operations WHERE hash=?",
                [hash.as_bytes().to_vec()],
            )
            .await?;
        Ok(match result.next().await? {
            Some(row) => Some(row.get::<String>(0)?),
            None => None,
        })
    }
    pub(super) async fn reject(&self, hash: Hash) -> Result<(), Error> {
        self.conn
            .execute(
                "UPDATE secure_operations SET status='rejected' WHERE hash=?",
                [hash.as_bytes().to_vec()],
            )
            .await?;
        Ok(())
    }
    /// Whether every dependency is applied or a member of `bundle`, which
    /// applies with it. A dependency whose frame was pruned counts through
    /// its log tip, its log's horizon or its stub.
    pub(super) async fn dependencies_ready(
        &self,
        frame: &EncryptedFrame,
        bundle: &BTreeSet<Hash>,
    ) -> Result<bool, Error> {
        let operation = frame.operation()?;
        let payload = self.row_payload(frame)?;
        if let Some((authored, _)) = payload.basis()
            && authored
                > self
                    .finalized_basis(operation.header.extensions.scope)
                    .await?
                    .map_or(0, |(sequence, _)| sequence)
        {
            return Ok(false);
        }

        let mut retired_creations = BTreeSet::new();
        if let Some(scope_id) = self
            .find_scope_id(operation.header.extensions.scope)
            .await?
            && let Some(finalized) = self
                .checkpoint_record(scope_id, true)
                .await?
                .and_then(|record| record.signed)
                .and_then(|signed| signed.statement.finalized)
            && let Some(key) = self.opening_key(
                operation.header.extensions.scope,
                operation.header.extensions.generation,
            )
            && let Ok(plaintext) = decrypt(
                &key.key,
                &encode(&operation.header.extensions)?,
                &frame.body,
            )
            && let Ok(payload) = RowPayload::decode(&plaintext)
        {
            let writes = payload.clone().into_writes();
            for (row, creation) in payload.creations() {
                let Some(write) = writes
                    .iter()
                    .find(|write| self.storage_row(&write.change).ok().as_ref() == Some(row))
                else {
                    return Err(denied("creation witness does not name a signed write"));
                };
                let hash = checkpoint::hash_of(creation)?;
                if write.change.before.is_none()
                    || !operation.header.previous.contains(&hash)
                        && operation.header.backlink != Some(hash)
                {
                    return Err(denied(
                        "creation witness is not an authenticated dependency",
                    ));
                }
                if self.row(write.change.scope, *row).await?.is_none()
                    && let Some(identity) = self
                        .app
                        .row_identity(&write.change)
                        .map_err(|error| denied(&error))?
                    && finalized.statement.scope == write.change.scope
                    && let Some(id) = self.find_scope_id(write.change.scope).await?
                    && let Some(checkpoint) = self
                        .checkpoint_record(id, true)
                        .await?
                        .and_then(|record| record.signed)
                    && checkpoint.statement.identities.iter().any(|mark| {
                        mark.author == identity.author() && mark.sequence >= identity.sequence()
                    })
                {
                    retired_creations.insert(hash);
                }
            }
        }
        for id in operation
            .header
            .previous
            .iter()
            .chain(operation.header.backlink.iter())
        {
            let Some(dependency) = self.frame(*id).await? else {
                match self.pruned_dependency(&operation.header, *id).await? {
                    Some(clock) if clock >= operation.header.timestamp => {
                        return Err(denied("wrong encrypted dependency scope/clock"));
                    }
                    Some(_) => continue,
                    None if retired_creations.contains(id)
                        && operation.header.backlink != Some(*id) =>
                    {
                        continue;
                    }
                    None => return Ok(false),
                }
            };
            let predecessor = dependency.operation()?;
            if predecessor.header.extensions.scope != operation.header.extensions.scope
                || predecessor.header.timestamp >= operation.header.timestamp
            {
                return Err(denied("wrong encrypted dependency scope/clock"));
            }
            if operation.header.backlink == Some(*id) {
                validate_backlink(&predecessor.header, &operation.header)
                    .map_err(|_| denied("invalid log predecessor"))?;
            }
            if !bundle.contains(id) && self.status(*id).await? != ReceiveStatus::Applied {
                return Ok(false);
            }
        }
        Ok(true)
    }
    /// The clock of an applied dependency whose frame is no longer retained,
    /// or `None` when the dependency is simply unknown.
    async fn pruned_dependency(
        &self,
        header: &p2panda_core::Header<Extensions>,
        id: Hash,
    ) -> Result<Option<u64>, Error> {
        let Some(scope) = self.find_scope_id(header.extensions.scope).await? else {
            return Ok(None);
        };
        if header.backlink == Some(id) {
            let Some(log) = self.log(scope, header.public_key).await? else {
                return Ok(None);
            };
            let predecessor = header.seq_num as i64 - 1;
            if predecessor == log.sequence {
                if log.hash != id {
                    return Err(denied("invalid log predecessor"));
                }
                return Ok(Some(log.clock));
            }
            // Deeper in the pruned prefix: covered, its clock is not kept.
            return Ok((predecessor <= log.horizon).then_some(0));
        }
        if let Some((stub, clock)) = self.stub(id).await? {
            if stub != scope {
                return Err(denied("wrong encrypted dependency scope/clock"));
            }
            return Ok(Some(clock));
        }
        // The pruned tip of another log of the scope.
        let mut tip = self
            .conn
            .query(
                "SELECT clock FROM secure_logs WHERE scope=? AND hash=? AND horizon>=sequence",
                bindings(vec![
                    SqlValue::Integer(scope),
                    SqlValue::Blob(id.as_bytes().to_vec()),
                ]),
            )
            .await?;
        match tip.next().await? {
            Some(row) => Ok(Some(row.get::<i64>(0)? as u64)),
            None => Ok(None),
        }
    }
}

pub(super) enum Outcome {
    Applied,
    Rejected(String),
    Waiting,
}
