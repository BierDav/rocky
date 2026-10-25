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
        #[cfg(test)]
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
        let payload = self.decode_row_payload(frame, &self.open_payload(frame)?)?;
        Ok(payload)
    }

    /// One bounded transaction of `EncryptedReplica::settle`; true when
    /// more frames may be ready.
    pub(super) async fn apply_step(&mut self, time: u64) -> Result<bool, Error> {
        let span = tracing::debug_span!(
            "apply",
            frames = tracing::field::Empty,
            rows = tracing::field::Empty,
            waiting = tracing::field::Empty,
            outcome = tracing::field::Empty
        );
        let (rows, frames) = (
            self.rows_written.load(Ordering::Relaxed),
            self.applied_frames,
        );
        let result = tracing::Instrument::instrument(
            async {
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
            },
            span.clone(),
        )
        .await;
        span.record(
            "rows",
            self.rows_written
                .load(Ordering::Relaxed)
                .saturating_sub(rows),
        );
        span.record("frames", self.applied_frames - frames);
        span.record("waiting", self.waits.len() as u64);
        crate::network::trace::outcome(&span, &result);
        result
    }
    /// Applies or rejects dependency-ready pending frames inside the caller's
    /// transaction, each application in its own savepoint. A failing
    /// application rolls back alone and is quarantined: its log waits and
    /// other logs go on. Only a storage failure ends the pass. Returns true
    /// when `limit` stopped the pass before it settled.
    ///
    /// Each later frame of a log names the one before as its backlink, so
    /// only a log's first pending frame can be ready: a round examines that
    /// frame of every log and follows a log while its frames apply, in
    /// sequence order across logs. A waiting log costs one examination per
    /// round, however many of its frames wait behind it.
    pub(super) async fn apply_pending(&mut self, time: u64, limit: usize) -> Result<bool, Error> {
        let mut changed = 0;
        let context = self.quarantine_context();
        let mut quarantined = self.quarantined().await?;
        let mut skipped = BTreeSet::new();
        loop {
            let mut heads = BTreeSet::new();
            for record in collect_rows(self.conn.query(
                "SELECT o.sequence,o.hash,o.scope,o.author FROM secure_operations o JOIN (SELECT scope,author,min(sequence) AS sequence FROM secure_operations WHERE status='pending' GROUP BY scope,author) h ON o.author=h.author AND o.scope=h.scope AND o.sequence=h.sequence",
                (),
            ).await?).await? {
                heads.insert(head(record)?);
            }
            let mut advanced = false;
            let mut examined = BTreeSet::new();
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
                let id = frame.id()?;
                examined.insert(id);
                let held = quarantined.get(&id).copied();
                if skipped.contains(&id) || held.is_some_and(|held| !held.due(time, context)) {
                    self.waits.insert(id, WaitReason::Quarantined);
                    continue;
                }
                self.conn.execute("SAVEPOINT quarantine", ()).await?;
                let outcome = Box::pin(self.apply_frame(&frame, time)).await;
                let failure = match &outcome {
                    Ok(Outcome::Waiting(WaitReason::Undecryptable)) => {
                        Some(quarantine::UNDECRYPTABLE)
                    }
                    Err(error) if quarantine::quarantines(error) => Some(error.category()),
                    _ => None,
                };
                if let Some(reason) = failure {
                    self.conn.execute("ROLLBACK TO quarantine", ()).await?;
                    self.conn.execute("RELEASE quarantine", ()).await?;
                    self.invalidate_context();
                    self.quarantine(id, reason, time, context).await?;
                    skipped.insert(id);
                    self.waits.insert(id, WaitReason::Quarantined);
                    continue;
                }
                self.conn.execute("RELEASE quarantine", ()).await?;
                if held.is_some() {
                    self.release_quarantine(id).await?;
                    quarantined.remove(&id);
                }
                match outcome {
                    Ok(Outcome::Waiting(reason)) => {
                        self.waits.insert(id, reason);
                        continue;
                    }
                    Ok(_) => {
                        self.waits.remove(&id);
                    }
                    Err(error) => {
                        if !retryable(&error) {
                            self.waits.insert(id, WaitReason::Failed);
                        }
                        return Err(error);
                    }
                }
                changed += 1;
                self.applied_frames += 1;
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
                // Every pending head was examined this round.
                self.waits.retain(|hash, _| examined.contains(hash));
                #[cfg(test)]
                if self.waits.len() != examined.len() {
                    return Err(Error::Corrupt("a pending frame has no wait reason".into()));
                }
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
        #[cfg(test)]
        if self.poisoned.contains(&operation.hash) {
            return Err(Error::Corrupt("poisoned frame".into()));
        }
        let reach = self.frame_reach(frame)?;
        if !self.frame_schema_ready(frame)? {
            return Ok(Outcome::Waiting(WaitReason::Schema));
        }
        // A sibling line is a refusal, never dependency work for this workspace.
        if reach == tips::FrameReach::Unfollowed {
            self.reject(operation.hash).await?;
            return Ok(Outcome::Rejected(
                "frame belongs to a tip this replica does not follow".into(),
            ));
        }
        let extension = &operation.header.extensions;
        // Verified checkpoint pages are filling the scope: its operations
        // apply on top once the import ends.
        if self.direct.contains(&extension.scope) {
            return Ok(Outcome::Waiting(WaitReason::DirectImport));
        }
        // Keyless custody does not owe retries for a key it may not receive.
        // Current read authorization is also required while an old key is held.
        if !self.can_read_key(
            extension.scope,
            &self.local_grants(time).unwrap_or_default(),
        ) {
            return Ok(Outcome::Waiting(WaitReason::ReadGrant));
        }
        // An authorized reader waits for an absent earlier or later epoch.
        let Some(key) = self.opening_key(extension.scope, extension.generation) else {
            return Ok(Outcome::Waiting(WaitReason::OpeningKey));
        };
        if reach == tips::FrameReach::Held
            && let Err(error) = self.verify_frame_authority(frame, time)
        {
            self.reject(operation.hash).await?;
            return Ok(Outcome::Rejected(error.to_string()));
        }
        if reach == tips::FrameReach::Held {
            if self.abandoned_write(frame)? {
                return self.hold_abandoned(frame, time).await;
            }
            return self.hold_closed_frame(frame, time).await;
        }
        // Every frame of a revoked author is refused, also one whose
        // dependencies, its author's earlier frames, never apply.
        if self
            .config
            .revocations
            .revoking(frame.chain()?.credentials())
            .map_err(credential)?
            .is_some()
        {
            self.reject(operation.hash).await?;
            return Ok(Outcome::Rejected("the frame's author is revoked".into()));
        }
        if !self
            .own_chain_ready(&operation.header, &BTreeSet::new())
            .await?
        {
            return Ok(Outcome::Waiting(WaitReason::OwnChain));
        }
        // The key its header names is held: a frame that does not open
        // under it is quarantined by the pass, not left waiting.
        let Ok(plaintext) = self.open_payload(frame) else {
            return Ok(Outcome::Waiting(WaitReason::Undecryptable));
        };
        let payload = self.decode_row_payload(frame, &plaintext);
        match self
            .dependencies_ready(frame, &BTreeSet::new(), payload.as_ref().ok())
            .await
        {
            Ok(true) => {}
            Ok(false) => return Ok(Outcome::Waiting(WaitReason::Dependencies)),
            Err(error) => {
                self.reject(operation.hash).await?;
                return Ok(Outcome::Rejected(error.to_string()));
            }
        }
        let grants = match self.verify_frame(frame, time) {
            Ok(grants) => grants,
            Err(error) => {
                self.reject(operation.hash).await?;
                return Ok(Outcome::Rejected(error.to_string()));
            }
        };
        if let Ok(payload) = &payload
            && let Some(membership) = payload.membership()
        {
            return Box::pin(self.apply_bundle(operation, membership, time)).await;
        }
        self.conn.execute("SAVEPOINT pending", ()).await?;
        let materialized = match &payload {
            Ok(payload) => {
                self.materialize(frame, payload.writes(), &grants, Some(payload))
                    .await
            }
            Err(error) => Err(group::shared(error)),
        };
        let result = match materialized {
            Ok((rows, admission)) => {
                async {
                    self.record_key(key).await?;
                    self.write_materialized(frame, rows, admission, payload.as_ref().ok())
                        .await?;
                    Ok(Outcome::Applied)
                }
                .await
            }
            Err(Error::Rejected(reason)) if reason == identity::LEDGER_WAITS => {
                self.conn.execute("ROLLBACK TO pending", ()).await?;
                self.invalidate_context();
                Ok(Outcome::Waiting(WaitReason::Ledger))
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
        opened: Option<&RowPayload>,
    ) -> Result<bool, Error> {
        let operation = frame.operation_ref()?;

        let decoded;
        let payload = match opened {
            Some(payload) => Some(payload),
            None => {
                decoded = self.row_payload(frame).ok();
                decoded.as_ref()
            }
        };
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
            && let Some(payload) = payload
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
    Waiting(WaitReason),
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
