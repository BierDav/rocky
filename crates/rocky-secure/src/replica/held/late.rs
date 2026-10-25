//! Original signed frames held after a line closes.
use super::*;
use crate::replica::incoming::Outcome;

impl<A: RowApplication> Inner<A> {
    /// Stores already authenticated held members in the caller's transaction.
    pub(in crate::replica) async fn hold_members(
        &self,
        frames: &[(EncryptedFrame, RowPayload)],
        time: u64,
    ) -> Result<(), Error> {
        let Some((first, payload)) = frames.first() else {
            return Err(denied("empty held bundle"));
        };
        let id = payload.membership().map_or_else(
            || first.id().map(|hash| *hash.as_bytes()),
            |membership| Ok(membership.transaction),
        )?;
        let names = self.item_names().await?;
        let local = self.local_grants(time)?;
        let mut stored = Stored {
            time: first.operation_ref()?.header.timestamp,
            rows: Vec::new(),
        };
        for (frame, payload) in frames {
            let operation = frame.operation_ref()?;
            let scope = operation.header.extensions.scope;
            let grants = self.verify_frame_authority(frame, time)?;
            if !self.can_read_key(scope, &local) || !self.holds(scope).await? {
                return Err(denied("held change requires a readable held scope"));
            }
            let writes = payload.clone().into_writes();
            if writes.is_empty() || writes.len() > ROWS_PER_FRAME {
                return Err(denied("held row change quota"));
            }
            let source = self
                .graph_for(scope.domain)?
                .schema_any(operation.header.extensions.tip)
                .map_err(credential)?;
            let definition = self.definition_at(&source).ok_or_else(|| {
                Error::Incompatible("held source tip schema is not installed".into())
            })?;
            let source_fields: BTreeSet<_> =
                definition.fields.iter().map(|field| field.id).collect();
            if definition.publisher != *self.config.publisher.as_bytes()
                || definition.app != scope.domain.app
                || !definition
                    .domains
                    .iter()
                    .any(|domain| domain.domain == scope.domain.domain)
                || !definition
                    .entities
                    .iter()
                    .any(|table| table.id == scope.table)
                || writes.iter().any(|write| {
                    write
                        .change
                        .before
                        .iter()
                        .chain(&write.change.after)
                        .any(|image| image.keys().any(|field| !source_fields.contains(field)))
                })
            {
                return Err(denied(
                    "held frame fields are outside its source tip schema",
                ));
            }
            let owned;
            let application: &dyn RowApplication = if source == self.manifest {
                self.app.as_ref()
            } else {
                owned = self
                    .app
                    .source_application(definition)
                    .map_err(Error::Incompatible)?;
                if owned.manifest().hash() != source {
                    return Err(Error::Incompatible(
                        "held source validator has a different manifest".into(),
                    ));
                }
                owned.as_ref()
            };
            let source_permissions = if source == self.manifest {
                BTreeSet::new()
            } else {
                application.permission_fields()
            };
            for write in &writes {
                if !write.valid()
                    || write.change.scope != scope
                    || !write.permitted_by(&grants)
                    || write.published_fields().iter().any(|field| {
                        self.permission_fields.contains(field) || source_permissions.contains(field)
                    })
                    || application
                        .row_partition(&write.change)
                        .map_err(|error| denied(&error))?
                        != operation.header.extensions.partition
                {
                    return Err(denied("held row/field transition denied"));
                }
                application
                    .validate_write(write)
                    .map_err(|error| denied(&error))?;
                let row = match application
                    .row_identity(&write.change)
                    .map_err(|error| denied(&error))?
                {
                    Some(id) => rocky_core::commitment(
                        "rocky.row-incarnation.v2",
                        &id.integer().to_le_bytes(),
                    ),
                    None => write.change.row,
                };
                stored.rows.push(stored_row(write, row, &names));
            }
            stored.time = stored.time.min(operation.header.timestamp);
        }
        let mut migration = false;
        let mut ledger_scopes = BTreeSet::new();
        for (frame, _) in frames {
            ledger_scopes.insert(frame.operation_ref()?.header.extensions.scope);
            migration |= !collect_rows(
                self.conn
                    .query(
                        "SELECT 1 FROM secure_migration_frames WHERE hash=?",
                        [frame.id()?.as_bytes().to_vec()],
                    )
                    .await?,
            )
            .await?
            .is_empty();
        }
        let resolved = collect_rows(
            self.conn
                .query(
                    "SELECT authority FROM secure_held_resolved WHERE id=?",
                    [id.to_vec()],
                )
                .await?,
        )
        .await?;
        let resolved = match resolved.first().map(Vec::as_slice) {
            Some([SqlValue::Blob(bytes)]) => {
                let grants: Vec<rocky_core::ResourceGrant> = decode(bytes)?;
                stored
                    .rows
                    .iter()
                    .all(|row| self.can_read_key(row.scope, &grants))
            }
            None => false,
            _ => return Err(Error::Corrupt("held resolution authority".into())),
        };
        if !resolved && !migration {
            self.conn
                .execute(
                    "INSERT OR REPLACE INTO secure_held VALUES(?,?)",
                    [id.to_vec(), encode(&stored)?],
                )
                .await?;
        }
        for (frame, _) in frames {
            let operation = frame.operation_ref()?;
            if !migration {
                self.conn
                    .execute(
                        "INSERT OR IGNORE INTO secure_held_frames VALUES(?,?,?,?)",
                        [
                            operation.hash.as_bytes().to_vec(),
                            id.to_vec(),
                            encode(&operation.header.extensions.scope)?,
                            encode(frame)?,
                        ],
                    )
                    .await?;
            }
            self.conn
                .execute(
                    "UPDATE secure_operations SET status='held' WHERE hash=?",
                    [operation.hash.as_bytes().to_vec()],
                )
                .await?;
            self.conn
                .execute(
                    "DELETE FROM secure_outbox WHERE hash=?",
                    [operation.hash.as_bytes().to_vec()],
                )
                .await?;
        }
        for scope in ledger_scopes {
            if let Some(id) = self.find_scope_id(scope).await? {
                self.reseed_ledger(scope, id).await?;
            }
        }
        self.conn
            .execute(
                "UPDATE secure_metadata SET revision=revision+1 WHERE id=1",
                (),
            )
            .await?;
        Ok(())
    }

    pub(in crate::replica) async fn hold_closed_frame(
        &self,
        frame: &EncryptedFrame,
        time: u64,
    ) -> Result<Outcome, Error> {
        let payload = self.row_payload(frame)?;
        if let Some(membership) = payload.membership() {
            return self.hold_closed_bundle(frame, membership, time).await;
        }
        self.hold_members(&[(frame.clone(), payload)], time).await?;
        Ok(Outcome::Applied)
    }

    /// Closed-source classification is lazy; learning a seal remains metadata only.
    pub(in crate::replica) async fn refresh_closed_held(&self, time: u64) -> Result<(), Error> {
        // Ordinary reach uses the transactional position index. Loading every
        // signed header here makes an empty held page scale with history size.
        let recovery = self.graphs.values().any(|graph| {
            graph
                .seals
                .iter()
                .any(|seal| seal.statement.recovery.is_some())
        });
        let mut context = tips::refresh::RefreshGraphContext::new(&self.graphs, self.workspace.tip);
        let records = rows(&self.conn, "SELECT o.hash,p.scope,p.tip,p.author,p.sequence FROM secure_operations o LEFT JOIN secure_position_sources p ON p.hash=o.hash AND p.kind=0 WHERE o.status IN ('applied','pending') ORDER BY p.scope,p.signer,p.log_sequence").await?;
        for record in records {
            let [
                SqlValue::Blob(hash),
                SqlValue::Blob(scope),
                SqlValue::Blob(tip),
                SqlValue::Integer(author),
                SqlValue::Integer(sequence),
            ] = record.as_slice()
            else {
                return Err(Error::Corrupt("closed source frame".into()));
            };
            if !recovery {
                let scope: ResourceScope = decode(scope)?;
                let tip = TipId(
                    tip.as_slice()
                        .try_into()
                        .map_err(|_| Error::Corrupt("closed source tip".into()))?,
                );
                let author = u32::try_from(*author)
                    .map_err(|_| Error::Corrupt("closed source author".into()))?;
                let sequence = u64::try_from(*sequence)
                    .map_err(|_| Error::Corrupt("closed source sequence".into()))?;
                let reach = self.frame_reach_at_using(
                    scope.domain,
                    tip,
                    author,
                    sequence,
                    0,
                    Some(&mut context),
                )?;
                if reach != tips::FrameReach::Held {
                    continue;
                }
            }
            let (header, body) = match collect_rows(
                self.conn
                    .query(
                        "SELECT header,body FROM secure_operations WHERE hash=?",
                        [hash.clone()],
                    )
                    .await?,
            )
            .await?
            .pop()
            .as_deref()
            {
                Some([SqlValue::Blob(header), SqlValue::Blob(body)]) => {
                    (header.clone(), body.clone())
                }
                _ => return Err(Error::Corrupt("closed source frame".into())),
            };
            let frame = self.stored_frame(header, body).await?;
            let operation = frame.operation_ref()?;
            let extension = &operation.header.extensions;
            if self.stored_status(operation.hash).await?.as_deref() == Some("held") {
                continue;
            }
            if self.frame_reach(&frame)? == tips::FrameReach::Held
                && self.can_read_key(extension.scope, &self.local_grants(time)?)
                && self
                    .opening_key(extension.scope, extension.generation)
                    .is_some()
            {
                if self.abandoned_write(&frame)? {
                    self.hold_abandoned(&frame, time).await?;
                } else {
                    self.hold_closed_frame(&frame, time).await?;
                }
            }
        }
        Ok(())
    }
    /// Holds the writes the workspace at `source` applied that `target`
    /// does not see, before a moved bookmark's workspace rebuild.
    pub(in crate::replica) async fn hold_line_writes(
        &self,
        scope: rocky_core::Scope,
        source: TipId,
        target: TipId,
    ) -> Result<usize, Error> {
        let graph = self.graph_for(scope)?;
        let mut line = BTreeSet::new();
        let mut cursor = source;
        while line.insert(cursor) {
            match Self::tip_parent(graph, cursor)? {
                Some((parent, _)) => cursor = parent,
                None => break,
            }
        }
        let records = rows(&self.conn,
            "SELECT header,body FROM secure_operations WHERE status IN ('applied','pending') ORDER BY scope,author,sequence",
        ).await?;
        let mut paths = BTreeMap::new();
        let mut held = 0;
        for record in records {
            let [SqlValue::Blob(header), SqlValue::Blob(body)] = record.as_slice() else {
                return Err(Error::Corrupt("held line frame".into()));
            };
            let frame = self.stored_frame(header.clone(), body.clone()).await?;
            let operation = frame.operation_ref()?;
            let extension = &operation.header.extensions;
            if extension.scope.domain != scope || !line.contains(&extension.tip) {
                continue;
            }
            let author = extension
                .author
                .ok_or_else(|| denied("node author number missing"))?;
            let path = match paths.entry(extension.tip) {
                std::collections::btree_map::Entry::Occupied(path) => path.into_mut(),
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(Self::frame_path(graph, extension.tip, target)?)
                }
            };
            if !path.admits(author, extension.author_sequence)?
                && self.stored_status(operation.hash).await?.as_deref() != Some("held")
            {
                match self.hold_closed_frame(&frame, now()?).await? {
                    Outcome::Applied => held += 1,
                    Outcome::Waiting(_) => {
                        return Err(denied("cannot leave an incomplete held bundle"));
                    }
                    Outcome::Rejected(reason) => return Err(denied(&reason)),
                }
            }
        }
        Ok(held)
    }

    pub(in crate::replica) async fn record_held_resolution(
        &self,
        id: Commitment,
        authority: &[rocky_core::ResourceGrant],
    ) -> Result<(), Error> {
        let held = collect_rows(
            self.conn
                .query("SELECT change FROM secure_held WHERE id=?", [id.to_vec()])
                .await?,
        )
        .await?;
        if let Some([SqlValue::Blob(bytes)]) = held.first().map(Vec::as_slice) {
            let stored: Stored = decode_bounded(bytes, MAX_BUNDLE_BYTES * 2)?;
            if stored
                .rows
                .iter()
                .any(|row| !self.can_read_key(row.scope, authority))
            {
                return Err(denied(
                    "held resolution requires read authority for every held scope",
                ));
            }
        }
        self.conn.execute("INSERT INTO secure_held_resolved VALUES(?,?) ON CONFLICT(id) DO UPDATE SET authority=excluded.authority", [id.to_vec(), encode(&authority)?]).await?;
        self.conn
            .execute("DELETE FROM secure_held WHERE id=?", [id.to_vec()])
            .await?;
        Ok(())
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// Checks read authority independently of the peer's followed tip.
    pub(crate) async fn authorize_held_reader(
        &self,
        peer: PublicKey,
        chain: &CredentialChain,
        scope: ResourceScope,
    ) -> Result<(), Error> {
        let inner = self.inner.lock().await;
        let grants = inner.verify_chain_scopes(chain, peer, now()?, &[scope])?;
        if !inner.can_read_key(scope, &grants) {
            return Err(denied("held changes require whole-scope read authority"));
        }
        Ok(())
    }
    pub(crate) async fn held_exchange_allowed(
        &self,
        peer: PublicKey,
        chain: &CredentialChain,
        scope: ResourceScope,
    ) -> Result<bool, Error> {
        let inner = self.inner.lock().await;
        let time = now()?;
        let grants = inner.verify_chain_scopes(chain, peer, time, &[scope])?;
        Ok(inner.can_read_key(scope, &grants)
            && inner.can_read_key(scope, &inner.local_grants(time)?)
            && inner.holds(scope).await?)
    }

    /// Original signed frames of unresolved held changes, ordered by hash.
    pub async fn held_frame_page(
        &self,
        peer: PublicKey,
        chain: &CredentialChain,
        scope: ResourceScope,
        after: Option<Hash>,
        quota: usize,
    ) -> Result<(Vec<EncryptedFrame>, Option<Hash>), Error> {
        if quota == 0 || quota > MAX_SECURE_FRAME {
            return Err(denied("held frame page quota"));
        }
        self.authorize_held_reader(peer, chain, scope).await?;
        let mut inner = self.inner.lock().await;
        transaction!(inner, |inner| inner.refresh_closed_held(now()?).await)?;
        inner.flush()?;
        let records = collect_rows(inner.conn.query(
            "SELECT f.hash,f.frame FROM secure_held_frames f JOIN secure_held h ON h.id=f.id WHERE f.scope=? AND f.hash>? ORDER BY f.hash LIMIT 33",
            [encode(&scope)?, after.map(|hash| hash.as_bytes().to_vec()).unwrap_or_default()],
        ).await?).await?;
        let mut frames: Vec<EncryptedFrame> = Vec::new();
        let mut size = 1024;
        for record in records {
            let [SqlValue::Blob(_), SqlValue::Blob(bytes)] = record.as_slice() else {
                return Err(Error::Corrupt("held source frame".into()));
            };
            let frame: EncryptedFrame = decode_bounded(bytes, MAX_SECURE_FRAME)?;
            if frames.len() == 32 || size + frame.weight() > quota {
                let Some(last) = frames.last() else {
                    return Err(denied("held frame does not fit the page quota"));
                };
                let next = last.id()?;
                return Ok((frames, Some(next)));
            }
            size += frame.weight();
            frames.push(frame);
        }
        Ok((frames, None))
    }
    /// Accepts only original frames whose line is closed. Ordinary live frames
    /// use `receive`; a relay never replaces their author's signature.
    pub async fn receive_held_frames(&self, frames: Vec<EncryptedFrame>) -> Result<(), Error> {
        {
            let inner = self.inner.lock().await;
            for frame in &frames {
                inner.verify_frame_authority(frame, now()?)?;
                if inner.frame_reach(frame)? != tips::FrameReach::Held {
                    return Err(denied("held transfer names a live or unrelated frame"));
                }
            }
        }
        for status in self.receive_all(frames).await {
            match status? {
                ReceiveStatus::Held
                | ReceiveStatus::Abandoned
                | ReceiveStatus::Duplicate
                | ReceiveStatus::PendingDependencies => {}
                _ => return Err(denied("held transfer was not held")),
            }
        }
        Ok(())
    }
}
