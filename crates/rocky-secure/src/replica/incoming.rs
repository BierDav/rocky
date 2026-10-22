use super::*;

impl<A: RowApplication> EncryptedReplica<A> {
    /// Applies pending frames one bounded transaction at a time, publishing
    /// after each commit. The replica is released between transactions, so
    /// a writer that asked for it meanwhile goes first. Applications before
    /// a failing one stay committed.
    pub(crate) async fn settle(&self) -> Result<(), Error> {
        loop {
            tokio::task::yield_now().await;
            #[cfg(test)]
            if !crate::apply_allowed() {
                return Err(Error::InjectedFailure);
            }
            if !self.inner.lock().await.apply_step(now()?).await? {
                return Ok(());
            }
        }
    }
}

impl<A: RowApplication> Inner<A> {
    pub(crate) fn decode_row_payload(
        &self,
        frame: &EncryptedFrame,
        bytes: &[u8],
    ) -> Result<RowPayload, Error> {
        let extension = &frame.operation_ref()?.header.extensions;
        let manifest = self.manifest_of_tip(extension.tip)?;
        let definition = self
            .catalog
            .get(&manifest)
            .or_else(|| (self.manifest == manifest).then_some(&self.definition))
            .ok_or_else(|| denied("source tip schema is not installed"))?;
        let mut payload = RowPayload::decode_source(bytes, definition, extension.scope)?;
        if !extension.births.is_empty()
            || payload.writes().iter().any(|write| {
                write.change.before.is_none() && payload.carried(&write.change.row).is_none()
            })
        {
            let owned;
            let application: &dyn RowApplication = if manifest == self.manifest {
                self.app.as_ref()
            } else {
                owned = self
                    .app
                    .source_application(definition)
                    .map_err(Error::Incompatible)?;
                if owned.manifest().hash() != manifest {
                    return Err(Error::Incompatible(
                        "source validator has a different manifest".into(),
                    ));
                }
                owned.as_ref()
            };
            payload.derive_births(frame, manifest, application)?;
        }
        Ok(payload)
    }
    pub(super) fn row_payload(&self, frame: &EncryptedFrame) -> Result<RowPayload, Error> {
        let operation = frame.operation_ref()?;
        #[cfg(test)]
        if crate::AUTHOR_PAYLOAD_FRAME.with(std::cell::Cell::get) == Some(operation.hash) {
            crate::AUTHOR_PAYLOAD_TRACE.with(|trace| {
                if let Some(trace) = trace.borrow_mut().as_mut()
                    && let Some((_, count)) = trace
                        .iter_mut()
                        .rev()
                        .find(|(seen, _)| seen.id().ok() == Some(operation.hash))
                {
                    *count += 1;
                }
            });
        }
        let extension = &operation.header.extensions;
        let key = self
            .opening_key(extension.scope, extension.generation)
            .ok_or_else(|| denied("row payload key unavailable"))?;
        let payload = self.decode_row_payload(
            frame,
            &decrypt(&key.key, &encode(extension)?, frame.body_bytes())?,
        )?;
        Ok(payload)
    }

    /// One bounded transaction of `EncryptedReplica::settle`; true when
    /// more frames may be ready.
    pub(super) async fn apply_step(&mut self, time: u64) -> Result<bool, Error> {
        let (more, failure) = transaction!(self, |inner| {
            match inner.apply_pending(time, group::GROUP_REQUESTS).await {
                Ok(more) => Ok((more, None)),
                Err(error) if retryable(&error) => Err(error),
                Err(error) => Ok((false, Some(error))),
            }
        })?;
        self.publish_revision().await?;
        match failure {
            Some(error) => Err(error),
            None => Ok(more),
        }
    }
    /// Applies or rejects dependency-ready pending frames inside the caller's
    /// transaction, each application in its own savepoint. A failing
    /// application rolls back alone and ends the pass with its error. Returns
    /// true when `limit` stopped the pass before it settled.
    ///
    /// Each later frame of a log names the one before as its backlink, so
    /// only a log's first pending frame can be ready: a round examines that
    /// frame of every log and follows a log while its frames apply, in
    /// sequence order across logs. A waiting log costs one examination per
    /// round, however many of its frames wait behind it.
    pub(super) async fn apply_pending(&mut self, time: u64, limit: usize) -> Result<bool, Error> {
        let mut changed = 0;
        loop {
            let mut heads = BTreeSet::new();
            for record in collect_rows(self.conn.query(
                "SELECT o.sequence,o.hash,o.scope,o.author FROM secure_operations o JOIN (SELECT scope,author,min(sequence) AS sequence FROM secure_operations WHERE status='pending' GROUP BY scope,author) h ON o.author=h.author AND o.scope=h.scope AND o.sequence=h.sequence",
                (),
            ).await?).await? {
                heads.insert(head(record)?);
            }
            let mut advanced = false;
            while let Some((sequence, hash, scope, author)) = heads.pop_first() {
                if changed >= limit {
                    return Ok(true);
                }
                let Some(frame) = self
                    .frame(Hash::from_bytes(
                        hash.try_into()
                            .map_err(|_| denied("corrupt pending operation"))?,
                    ))
                    .await?
                else {
                    return Err(denied("missing pending operation"));
                };
                if matches!(
                    Box::pin(self.apply_frame(&frame, time)).await?,
                    Outcome::Waiting
                ) {
                    continue;
                }
                changed += 1;
                advanced = true;
                let next = collect_rows(self.conn.query(
                    "SELECT sequence,hash,scope,author FROM secure_operations WHERE scope=? AND author=? AND sequence>? AND status='pending' ORDER BY sequence LIMIT 1",
                    bindings(vec![SqlValue::Blob(scope), SqlValue::Blob(author), SqlValue::Integer(sequence)]),
                ).await?).await?;
                for record in next {
                    heads.insert(head(record)?);
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
        let operation = frame.operation_ref()?;
        // A bundle applies its members together; a later scan finds them settled.
        match self.stored_status(operation.hash).await?.as_deref() {
            Some("applied" | "abandoned" | "held") => return Ok(Outcome::Applied),
            Some("rejected") => return Ok(Outcome::Rejected("rejected earlier".into())),
            _ => {}
        }
        let reach = self.frame_reach(frame)?;
        if !self.frame_schema_ready(frame)? {
            return Ok(Outcome::Waiting);
        }
        let grants = match if reach == tips::FrameReach::Held {
            self.verify_frame_authority(frame, time)
        } else {
            self.verify_frame(frame, time)
        } {
            Ok(grants) => grants,
            Err(error) => {
                self.reject(operation.hash).await?;
                return Ok(Outcome::Rejected(error.to_string()));
            }
        };
        let extension = &operation.header.extensions;
        // Verified checkpoint pages are filling the scope: its operations
        // apply on top once the import ends.
        if self.direct.contains(&extension.scope) {
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
        if reach == tips::FrameReach::Held {
            if self.abandoned_write(frame)? {
                return self.hold_abandoned(frame, time).await;
            }
            return self.hold_closed_frame(frame, time).await;
        }
        if !self
            .own_chain_ready(&operation.header, &BTreeSet::new())
            .await?
        {
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
            frame.body_bytes(),
        );
        let Ok(plaintext) = plaintext else {
            return Ok(Outcome::Waiting);
        };
        let payload = self.decode_row_payload(frame, &plaintext);
        if let Ok(payload) = &payload
            && let Some(membership) = payload.membership()
        {
            return Box::pin(self.apply_bundle(operation, membership, time)).await;
        }
        self.conn.execute("SAVEPOINT pending", ()).await?;
        let materialized = match payload {
            Ok(payload) => {
                self.materialize(frame, &payload.into_writes(), &grants, None)
                    .await
            }
            Err(error) => Err(error),
        };
        let result = match materialized {
            Ok((rows, admission)) => {
                async {
                    self.record_key(key).await?;
                    self.write_materialized(frame, rows, admission, None)
                        .await?;
                    Ok(Outcome::Applied)
                }
                .await
            }
            Err(Error::Rejected(reason)) if reason == identity::LEDGER_WAITS => {
                self.conn.execute("ROLLBACK TO pending", ()).await?;
                self.invalidate_context();
                Ok(Outcome::Waiting)
            }
            Err(error) if identity::receipts::semantic_refusal(&error) => {
                self.conn.execute("ROLLBACK TO pending", ()).await?;
                self.invalidate_context();
                self.reject(operation.hash)
                    .await
                    .map(|()| Outcome::Rejected(error.to_string()))
            }
            Err(error) => Err(error),
        };
        if result.is_err() {
            self.conn.execute("ROLLBACK TO pending", ()).await?;
            self.invalidate_context();
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
    /// its log tip, its log's pruned_prefix or its stub.
    pub(super) async fn dependencies_ready(
        &self,
        frame: &EncryptedFrame,
        bundle: &BTreeSet<Hash>,
    ) -> Result<bool, Error> {
        let operation = frame.operation_ref()?;

        let mut retired_creations = BTreeSet::new();
        // Behind a finalized base, a creation whose row is gone is retired.
        // A frame of another line stays as it was applied there.
        if self.on_workspace_path(
            operation.header.extensions.scope.domain,
            operation.header.extensions.tip,
        )? && let Some(scope_id) = self
            .find_scope_id(operation.header.extensions.scope)
            .await?
            && let Some(finalized) = self
                .checkpoint_record(scope_id, true)
                .await?
                .and_then(|record| record.signed)
                .filter(|signed| signed.statement.seal.is_some())
            && let Some(key) = self.opening_key(
                operation.header.extensions.scope,
                operation.header.extensions.generation,
            )
            && let Ok(plaintext) = decrypt(
                &key.key,
                &encode(&operation.header.extensions)?,
                frame.body_bytes(),
            )
            && let Ok(payload) = self.decode_row_payload(frame, &plaintext)
        {
            // The live schema names rows through the path's fields.
            let fields = self.path_fields(
                operation.header.extensions.scope.domain,
                operation.header.extensions.tip,
            )?;
            let writes: Vec<_> = payload
                .clone()
                .into_writes()
                .iter()
                .filter_map(|write| fields.project(write))
                .collect();
            // A table off the path applies nothing, so it creates nothing.
            let creations = if fields
                .tables
                .contains(&operation.header.extensions.scope.table)
            {
                payload.creations()
            } else {
                Default::default()
            };
            for (row, creation) in creations {
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
                    None => match self.voided_dependency(&operation.header, *id).await? {
                        Some(true) => continue,
                        Some(false) | None => return Ok(false),
                    },
                }
            };
            let predecessor = dependency.operation_ref()?;
            if predecessor.header.extensions.scope != operation.header.extensions.scope
                || predecessor.header.timestamp >= operation.header.timestamp
            {
                return Err(denied("wrong encrypted dependency scope/clock"));
            }
            if operation.header.backlink == Some(*id) {
                validate_backlink(&predecessor.header, &operation.header)
                    .map_err(|_| denied("invalid log predecessor"))?;
            }
            if !bundle.contains(id) {
                match self.status(*id).await? {
                    ReceiveStatus::Applied | ReceiveStatus::Abandoned => {}
                    ReceiveStatus::Held if self.visible_held_in_workspace(predecessor)? => {}
                    _ => return Ok(false),
                }
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
            return Ok((predecessor <= log.pruned_prefix).then_some(0));
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
                "SELECT clock FROM secure_logs WHERE scope=? AND hash=? AND pruned_prefix>=sequence",
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

/// A log's first pending frame: sequence, hash, scope and author.
type Head = (i64, Vec<u8>, Vec<u8>, Vec<u8>);
fn head(record: Vec<SqlValue>) -> Result<Head, Error> {
    match <[SqlValue; 4]>::try_from(record) {
        Ok(
            [
                SqlValue::Integer(sequence),
                SqlValue::Blob(hash),
                SqlValue::Blob(scope),
                SqlValue::Blob(author),
            ],
        ) => Ok((sequence, hash, scope, author)),
        _ => Err(denied("corrupt pending operation")),
    }
}
