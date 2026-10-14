//! Checkpoint creation in bounded steps: capture, replay, seal, install.
use super::*;

pub(in crate::replica) enum Step {
    /// The latest verified checkpoint; nothing is in progress.
    Done(Box<SignedCheckpoint>),
    Began,
    More,
    Idle,
}

fn chain(label: &str, items: &[Vec<SqlValue>]) -> Result<SetSummary, Error> {
    let mut bytes = Vec::new();
    for item in items {
        for value in item {
            match value {
                SqlValue::Blob(value) => {
                    bytes.extend((value.len() as u64).to_le_bytes());
                    bytes.extend(value);
                }
                SqlValue::Integer(value) => bytes.extend(value.to_le_bytes()),
                _ => return Err(Error::Corrupt("checkpoint summary column".into())),
            }
        }
    }
    Ok(SetSummary {
        count: items.len() as u64,
        digest: rocky_core::commitment(label, &bytes),
    })
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// Brings the scope's checkpoint up to its applied operations and returns
    /// it. Each step is its own transaction and other work may run between
    /// steps. The checkpoint is used only after replaying the retained
    /// operations over the previous checkpoint reproduced the stored rows.
    pub async fn checkpoint(&self, scope: ResourceScope) -> Result<SignedCheckpoint, Error> {
        let mut begun = false;
        loop {
            let mut inner = self.inner.lock().await;
            match inner.checkpoint_step(scope, !begun).await? {
                Step::Done(signed) => return Ok(*signed),
                Step::Began => begun = true,
                Step::More => {}
                // Automatic maintenance finished it between two steps.
                Step::Idle => begun = false,
            }
        }
    }
}

impl<A: RowApplication> Inner<A> {
    pub(in crate::replica) async fn checkpoint_step(
        &mut self,
        scope: ResourceScope,
        may_begin: bool,
    ) -> Result<Step, Error> {
        let (generation, _) = self.scope_key(scope)?;
        let record = match self.find_scope_id(scope).await? {
            Some(id) => self
                .checkpoint_record(id, false)
                .await?
                .map(|record| (id, record)),
            None => None,
        };
        match record {
            Some((id, record)) if record.state == "building" => {
                if record.cursor < record.position {
                    Box::pin(self.replay_step(scope, id, record)).await
                } else {
                    Box::pin(self.seal_step(scope, id, generation, record)).await
                }
            }
            Some((id, record)) if record.state == "installing" => {
                Box::pin(self.install_step(scope, id, record)).await
            }
            _ if may_begin => Box::pin(self.begin_step(scope)).await,
            _ => Ok(Step::Idle),
        }
    }
    /// Finishes an install so the checkpoint rows are whole again.
    pub(in crate::replica) async fn settle(&mut self, scope: ResourceScope) -> Result<(), Error> {
        let Some(id) = self.find_scope_id(scope).await? else {
            return Ok(());
        };
        while let Some(record) = self.checkpoint_record(id, false).await?
            && record.state == "installing"
        {
            Box::pin(self.install_step(scope, id, record)).await?;
        }
        Ok(())
    }
    async fn begin_step(&mut self, scope: ResourceScope) -> Result<Step, Error> {
        // Verified pages are still arriving; the import's own checkpoint
        // becomes the scope's when it ends.
        if self.direct.contains(&scope) {
            return Err(denied("the scope is being imported from a checkpoint"));
        }
        // A checkpoint covering an uncommitted entry could not roll it back.
        if self.staged_in(scope).await? {
            return Err(denied("a coordinated entry in the scope is undecided"));
        }
        // Nothing is sealed over a base holding abandoned effects.
        self.ensure_cut_clean(scope).await?;
        if self.cut_pending(scope.domain).await? {
            return Err(denied("a cut of the scope's log is rolling back"));
        }
        if let Some(id) = self.find_scope_id(scope).await?
            && let Some(base) = self
                .checkpoint_record(id, true)
                .await?
                .and_then(|record| record.signed)
        {
            self.check_binding(&base.statement)?;
        }
        transaction!(self, |inner| {
            let id = inner.scope_id(scope).await?;
            let verified = inner.checkpoint_record(id, true).await?;
            let target = match collect_rows(
                inner
                    .conn
                    .query(
                        "SELECT COALESCE(MAX(position),0) FROM secure_applied WHERE scope=?",
                        [id],
                    )
                    .await?,
            )
            .await?
            .first()
            .map(Vec::as_slice)
            {
                Some([SqlValue::Integer(target)]) => *target,
                _ => return Err(Error::Corrupt("applied positions".into())),
            };
            // Nothing new: the current checkpoint stands, unless it was signed
            // under other revocations or key epoch, or imported from another signer.
            if let Some(record) = &verified
                && target <= record.position
            {
                let signed = inner.verify_record(scope, record)?;
                let signer = inner
                    .row_authors
                    .get(&scope.table)
                    .map(|author| &author.key)
                    .or(inner.credential_signer.as_ref())
                    .unwrap_or(&inner.config.node)
                    .public_key();
                if signed.signer() == signer
                    && signed.statement.revocations == inner.revocation_digest()?
                    && signed.statement.key_epoch == inner.scope_key(scope)?.0
                    && signed.statement.strong.head == inner.capture_strong(scope).await?.head
                    && signed.statement.horizon == inner.mergeable_horizon(scope).await?
                {
                    return Ok(Step::Done(Box::new(signed)));
                }
            }
            let logs = inner.logs(id, None, false, MAX_CHECKPOINT_LOGS + 1).await?;
            if logs.len() > MAX_CHECKPOINT_LOGS {
                return Err(Error::Quota {
                    resource: "encrypted checkpoint logs",
                    limit: MAX_CHECKPOINT_LOGS,
                });
            }
            let horizon = inner.mergeable_horizon(scope).await?;
            if horizon.as_ref().is_some_and(|signed| {
                signed.statement.logs.iter().any(|cut| {
                    !logs.iter().any(|(log, _)| {
                        log.author == cut.signer
                            && log.sequence >= cut.sequence
                            && (log.sequence != cut.sequence || log.hash.as_bytes() == &cut.head)
                    })
                })
            }) {
                return Err(denied(
                    "sync to the admitted issuer horizon before capturing a checkpoint",
                ));
            }
            if verified
                .as_ref()
                .and_then(|record| record.signed.as_ref())
                .is_some_and(|signed| signed.statement.finalized.is_some())
            {
                inner
                    .capture_checkpoint_creations(scope, id, "secure_checkpoint_build_stubs")
                    .await?;
            }
            let (rows, tombstones, digest, applied) = inner.scope_state(id).await?;
            let (handles, handle_digest) = inner.handle_set().await?;
            let previous = match &verified {
                Some(record) => Some(inner.verify_record(scope, record)?.id()?),
                None => None,
            };
            let capture = Capture {
                finalized: verified
                    .as_ref()
                    .and_then(|record| record.signed.as_ref())
                    .and_then(|signed| signed.statement.finalized.clone()),
                horizon,
                identities: inner.identity_marks(scope).await?,
                clock: logs.iter().map(|(log, _)| log.clock).max().unwrap_or(0),
                frontier: logs.into_iter().map(|(log, _)| log).collect(),
                operations: applied,
                rows: SetSummary {
                    count: rows,
                    digest: digest.digest(),
                },
                tombstones,
                handles: SetSummary {
                    count: handles,
                    digest: handle_digest.digest(),
                },
                previous,
            };
            let (cursor, sets) = match verified {
                Some(record) => (record.position, record.sets),
                None => (0, Sets::new()),
            };
            inner
                .conn
                .execute("DELETE FROM secure_checkpoint_overlay WHERE scope=?", [id])
                .await?;
            inner
                .conn
                .execute(
                    "DELETE FROM secure_checkpoints WHERE scope=? AND state<>'verified'",
                    [id],
                )
                .await?;
            inner
                .conn
                .execute(
                    "INSERT INTO secure_checkpoints SELECT ?,COALESCE(MAX(sequence),0)+1,'building',?,?,?,?,NULL FROM secure_checkpoints WHERE scope=?",
                    bindings(vec![
                        SqlValue::Integer(id),
                        SqlValue::Integer(target),
                        SqlValue::Integer(cursor),
                        SqlValue::Blob(sets.to_bytes()),
                        SqlValue::Blob(encode(&capture)?),
                        SqlValue::Integer(id),
                    ]),
                )
                .await?;
            inner.fault(Stage::Begin)?;
            Ok(Step::Began)
        })
    }
    async fn replay_step(
        &mut self,
        scope: ResourceScope,
        id: i64,
        mut record: Record,
    ) -> Result<Step, Error> {
        transaction!(self, |inner| {
            let last = inner
                .replay(
                    scope,
                    id,
                    Overlay::Build,
                    (record.cursor, record.position),
                    STEP_OPERATIONS,
                    &mut record.sets,
                )
                .await?;
            inner
                .conn
                .execute(
                    "UPDATE secure_checkpoints SET cursor=?,sets=? WHERE scope=? AND sequence=?",
                    bindings(vec![
                        SqlValue::Integer(last.unwrap_or(record.position)),
                        SqlValue::Blob(record.sets.to_bytes()),
                        SqlValue::Integer(id),
                        SqlValue::Integer(record.sequence),
                    ]),
                )
                .await?;
            inner.fault(Stage::Replay)?;
            Ok(Step::More)
        })
    }
    /// Compares the replay with the capture, then signs the statement.
    async fn seal_step(
        &mut self,
        scope: ResourceScope,
        id: i64,
        generation: rocky_core::KeyEpoch,
        record: Record,
    ) -> Result<Step, Error> {
        // A cut admitted since the capture may abandon what it replayed.
        if self.cut_pending(scope.domain).await? {
            transaction!(self, |inner| {
                inner
                    .conn
                    .execute(
                        "UPDATE secure_checkpoints SET state='failed' WHERE scope=? AND sequence=?",
                        [id, record.sequence],
                    )
                    .await?;
                inner
                    .conn
                    .execute("DELETE FROM secure_checkpoint_overlay WHERE scope=?", [id])
                    .await?;
                Ok(())
            })?;
            return Err(denied("a cut of the scope's log is rolling back"));
        }
        let verified = transaction!(self, |inner| {
            let capture = record
                .capture
                .clone()
                .ok_or_else(|| Error::Corrupt("checkpoint capture".into()))?;
            if record.sets.rows_summary() != capture.rows
                || record.sets.tombstones != capture.tombstones
            {
                inner
                    .conn
                    .execute(
                        "UPDATE secure_checkpoints SET state='failed' WHERE scope=? AND sequence=?",
                        [id, record.sequence],
                    )
                    .await?;
                inner
                    .conn
                    .execute("DELETE FROM secure_checkpoint_overlay WHERE scope=?", [id])
                    .await?;
                return Ok(false);
            }
            let previous = inner.checkpoint_record(id, true).await?;
            let manifest = &inner.definition;
            let horizon = capture.horizon;
            let finalized = capture.finalized;
            let statement = CheckpointStatement {
                format: CHECKPOINT_FORMAT,
                finalized,
                horizon,
                partition: inner.partitions.get(&scope).cloned(),
                identities: capture.identities,
                moves: inner.checkpoint_moves(id).await?,
                definition: inner.manifest,
                schema: manifest.schema,
                handler: manifest.handler,
                revocations: inner.revocation_digest()?,
                scope,
                key_epoch: generation,
                sequence: previous
                    .and_then(|record| record.signed)
                    .map_or(0, |signed| signed.statement.sequence)
                    + 1,
                previous: capture.previous,
                frontier: capture.frontier,
                clock: capture.clock,
                operations: capture.operations,
                rows: capture.rows,
                tombstones: capture.tombstones,
                creations: record.sets.creations_summary(),
                handles: capture.handles,
                strong: inner.strong_summary(scope).await?,
                custody: inner.custody_summary(id, record.position).await?,
                coordinated: inner.seal_binding(scope, id, record.position).await?,
            };
            let strong_state = inner.capture_strong(scope).await?;
            inner.conn.execute("INSERT INTO secure_checkpoint_strong VALUES(?,?) ON CONFLICT(scope) DO UPDATE SET state=excluded.state", bindings(vec![SqlValue::Integer(id),SqlValue::Blob(encode(&strong_state)?)])).await?;
            let signed = inner.sign_checkpoint(statement)?;
            inner
                .conn
                .execute(
                    "UPDATE secure_checkpoints SET state='installing',signed=? WHERE scope=? AND sequence=?",
                    bindings(vec![
                        SqlValue::Blob(signed.to_bytes()?),
                        SqlValue::Integer(id),
                        SqlValue::Integer(record.sequence),
                    ]),
                )
                .await?;
            inner.fault(Stage::Seal)?;
            Ok(true)
        })?;
        if !verified {
            return Err(Error::Corrupt(
                "checkpoint verification failed: replaying retained operations over the previous checkpoint does not reproduce the stored rows".into(),
            ));
        }
        Ok(Step::More)
    }
    /// Moves verified overlay rows into the checkpoint rows; the last step
    /// makes the checkpoint current.
    async fn install_step(
        &mut self,
        scope: ResourceScope,
        id: i64,
        record: Record,
    ) -> Result<Step, Error> {
        let done = transaction!(self, |inner| {
            let rows = collect_rows(
                inner
                    .conn
                    .query(
                        "SELECT row,fields FROM secure_checkpoint_overlay WHERE scope=? ORDER BY row LIMIT ?",
                        [id, STEP_ROWS as i64],
                    )
                    .await?,
            )
            .await?;
            let done = rows.len() < STEP_ROWS;
            for row in rows {
                let [SqlValue::Blob(row), SqlValue::Blob(fields)] = row.as_slice() else {
                    return Err(Error::Corrupt("checkpoint overlay row".into()));
                };
                for sql in [
                    "INSERT OR REPLACE INTO secure_checkpoint_rows VALUES(?,?,?)",
                    "DELETE FROM secure_checkpoint_overlay WHERE scope=? AND row=? AND fields=?",
                ] {
                    inner
                        .conn
                        .execute(
                            sql,
                            bindings(vec![
                                SqlValue::Integer(id),
                                SqlValue::Blob(row.clone()),
                                SqlValue::Blob(fields.clone()),
                            ]),
                        )
                        .await?;
                }
            }
            if done {
                inner
                    .conn
                    .execute("DELETE FROM secure_checkpoint_stubs WHERE scope=?", [id])
                    .await?;
                inner.conn.execute("INSERT INTO secure_checkpoint_stubs SELECT * FROM secure_checkpoint_build_stubs WHERE scope=?", [id]).await?;
                inner
                    .conn
                    .execute(
                        "DELETE FROM secure_checkpoint_build_stubs WHERE scope=?",
                        [id],
                    )
                    .await?;
                inner
                    .conn
                    .execute(
                        "UPDATE secure_checkpoints SET state='verified' WHERE scope=? AND sequence=?",
                        [id, record.sequence],
                    )
                    .await?;
                inner
                    .conn
                    .execute(
                        "DELETE FROM secure_checkpoints WHERE scope=? AND sequence<?",
                        [id, record.sequence],
                    )
                    .await?;
                // Its own checkpoint's pages are signed as they are served.
                inner
                    .conn
                    .execute("DELETE FROM secure_checkpoint_pages WHERE scope=?", [id])
                    .await?;
            }
            inner.fault(Stage::Install)?;
            Ok(done)
        })?;
        if !done {
            return Ok(Step::More);
        }
        self.verified_at.insert(id, self.commit);
        let record = self
            .checkpoint_record(id, true)
            .await?
            .ok_or_else(|| Error::Corrupt("installed checkpoint".into()))?;
        Ok(Step::Done(Box::new(self.verify_record(scope, &record)?)))
    }
    /// Strong artifacts are bound by identity; none of them is pruned.
    pub(in crate::replica) async fn strong_summary(
        &self,
        scope: ResourceScope,
    ) -> Result<StrongSummary, Error> {
        let head = collect_rows(
            self.conn
                .query(
                    "SELECT sequence,head,rows FROM secure_command_rows WHERE scope=?",
                    [encode(&scope)?],
                )
                .await?,
        )
        .await?;
        let (head, row_commitment) = match head.first().map(Vec::as_slice) {
            Some(
                [
                    SqlValue::Integer(sequence),
                    SqlValue::Blob(head),
                    SqlValue::Blob(rows),
                ],
            ) => (
                Some((*sequence as u64, row_of(head)?)),
                rocky_core::commitment("rocky.checkpoint-command-rows.v1", rows),
            ),
            _ => (
                None,
                rocky_core::commitment(
                    "rocky.checkpoint-command-rows.v1",
                    &encode(&BTreeMap::<Commitment, rocky_core::RowFields>::new())?,
                ),
            ),
        };
        let results = collect_rows(
            self.conn
                .query(
                    "SELECT sequence,LENGTH(result) FROM secure_results WHERE domain=? AND sequence>? ORDER BY sequence",
                    bindings(vec![SqlValue::Blob(encode(&scope.domain)?),SqlValue::Integer(head.map_or(0, |head| head.0) as i64)]),
                )
                .await?,
        )
        .await?;
        let mut commands = Vec::new();
        for record in rows(&self.conn,"SELECT handle,input,command FROM secure_commands UNION ALL SELECT handle,input,command FROM secure_recovered_commands ORDER BY handle").await? {
            let [SqlValue::Blob(handle),SqlValue::Blob(input),SqlValue::Blob(bytes)] = record.as_slice() else { return Err(denied("command summary")); };
            let request = EncryptedCommand::from_bytes(bytes)?.request()?;
            if request.scope != scope.domain { continue; }
            let (state,sequence) = self.progress_of(&request.hash()?).await?;
            if ![3,4,5,6].contains(&state) || sequence.is_some_and(|sequence| sequence > head.map_or(0, |head| head.0)) {
                commands.push(vec![SqlValue::Blob(handle.clone()),SqlValue::Blob(input.clone())]);
            }
        }
        let mut preparations = Vec::new();
        for record in rows(
            &self.conn,
            "SELECT request,result FROM secure_preparations ORDER BY request",
        )
        .await?
        {
            let [SqlValue::Blob(key), SqlValue::Blob(bytes)] = record.as_slice() else {
                return Err(denied("preparation summary"));
            };
            let package = ConfirmedCommand::from_bytes(bytes)?.strong_package()?;
            let (state, _) = self.progress_of(&package.request.hash()?).await?;
            if package.request.scope == scope.domain
                && (package.sequence > head.map_or(0, |head| head.0)
                    || ![3, 4, 5, 6].contains(&state))
            {
                preparations.push(vec![SqlValue::Blob(key.clone())]);
            }
        }
        Ok(StrongSummary {
            base: head,
            head,
            rows: row_commitment,
            results: chain("rocky.checkpoint-results.v1", &results)?,
            commands: chain("rocky.checkpoint-commands.v1", &commands)?,
            preparations: chain("rocky.checkpoint-preparations.v1", &preparations)?,
            observations: self
                .summary(
                    "rocky.checkpoint-observations.v1",
                    "SELECT id,job FROM secure_effect_observations ORDER BY id",
                )
                .await?,
        })
    }
    async fn summary(&self, label: &str, sql: &str) -> Result<SetSummary, Error> {
        chain(label, &rows(&self.conn, sql).await?)
    }
    pub(in crate::replica) async fn custody_summary(
        &self,
        id: i64,
        position: i64,
    ) -> Result<CustodySummary, Error> {
        let superseded = collect_rows(
            self.conn
                .query(
                    "SELECT COUNT(*) FROM secure_applied a WHERE a.scope=? AND a.position<=? AND EXISTS(SELECT 1 FROM secure_ack k WHERE k.hash=a.hash)",
                    [id, position],
                )
                .await?,
        )
        .await?;
        let Some([SqlValue::Integer(superseded)]) = superseded.first().map(Vec::as_slice) else {
            return Err(Error::Corrupt("custody summary".into()));
        };
        let sole = collect_rows(
            self.conn
                .query(
                    "SELECT a.hash FROM secure_applied a WHERE a.scope=? AND a.position<=? AND NOT EXISTS(SELECT 1 FROM secure_ack k WHERE k.hash=a.hash) ORDER BY a.position",
                    [id, position],
                )
                .await?,
        )
        .await?;
        Ok(CustodySummary {
            superseded: *superseded as u64,
            sole: chain("rocky.checkpoint-sole-copies.v1", &sole)?,
            archives: chain(
                "rocky.checkpoint-archives.v1",
                &rows(
                    &self.conn,
                    "SELECT id FROM secure_recovery_custody ORDER BY id",
                )
                .await?,
            )?,
        })
    }
    /// One bounded step of automatic maintenance for the scopes a group
    /// wrote: continue or start a checkpoint, else prune one batch. The
    /// group is already committed, so a failure is kept for the status.
    pub(crate) async fn maintain(&mut self, scopes: &BTreeSet<ResourceScope>) {
        for domain in scopes
            .iter()
            .map(|scope| scope.domain)
            .collect::<BTreeSet<_>>()
        {
            if let Err(error) = self.prune_strong(domain).await {
                self.maintenance_error = Some(error.to_string());
                return;
            }
        }
        for scope in scopes {
            match self.maintain_scope(*scope).await {
                Ok(true) => {
                    self.maintenance_error = None;
                    return;
                }
                Ok(false) => {}
                Err(error) => {
                    self.maintenance_error = Some(error.to_string());
                    return;
                }
            }
        }
    }
    /// True when a step ran. A scope being imported waits: its own
    /// checkpoint would only repeat the one it imports.
    async fn maintain_scope(&mut self, scope: ResourceScope) -> Result<bool, Error> {
        if self.scope_key(scope).is_err() || self.importing(scope).await? {
            return Ok(false);
        }
        let Some(id) = self.find_scope_id(scope).await? else {
            return Ok(false);
        };
        let latest = self.checkpoint_record(id, false).await?;
        let state = latest.as_ref().map_or("", |record| record.state.as_str());
        if ["building", "installing"].contains(&state) {
            self.checkpoint_step(scope, false).await?;
            return Ok(true);
        }
        let verified = self
            .checkpoint_record(id, true)
            .await?
            .and_then(|record| record.signed);
        // Revocations and key epochs both fence transfer of an older checkpoint.
        let digest = self.revocation_digest()?;
        let rotated = verified.as_ref().is_some_and(|signed| {
            signed.statement.revocations != digest
                || self
                    .scope_key(scope)
                    .is_ok_and(|(generation, _)| generation != signed.statement.key_epoch)
        });
        let horizon_changed = verified
            .as_ref()
            .and_then(|signed| signed.statement.horizon.as_ref())
            != self.mergeable_horizon(scope).await?.as_ref();
        if (rotated || horizon_changed) && state != "failed" {
            self.checkpoint_step(scope, true).await?;
            return Ok(true);
        }
        let covered = verified.map_or(0, |signed| signed.statement.operations);
        let uncovered = self.scope_state(id).await?.3.saturating_sub(covered);
        let pressed = self.retained().await?.1 >= self.limits.soft_bytes as u64;
        // A failed verification is not retried automatically.
        if state != "failed"
            && (uncovered >= self.limits.checkpoint_operations as u64 || (pressed && uncovered > 0))
        {
            self.checkpoint_step(scope, true).await?;
            return Ok(true);
        }
        if state == "verified" && self.retention_policy(scope).await?.is_some() {
            return Ok(self.prune_step(scope).await?.pruned > 0);
        }
        Ok(false)
    }
}
