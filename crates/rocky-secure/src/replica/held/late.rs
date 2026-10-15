//! Original signed frames held after a line closes.
use super::*;
use crate::replica::incoming::Outcome;
use rocky_credentials::AuthorMark;

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
            time: first.operation()?.header.timestamp,
            rows: Vec::new(),
        };
        for (frame, payload) in frames {
            let operation = frame.operation()?;
            let scope = operation.header.extensions.scope;
            let grants = self.verify_frame_authority(frame, time)?;
            if !self.can_read_key(scope, &local) || !self.holds(scope).await? {
                return Err(denied("held change requires a readable held scope"));
            }
            let writes = payload.clone().into_writes();
            if writes.is_empty() || writes.len() > ROWS_PER_FRAME {
                return Err(denied("held row change quota"));
            }
            for write in &writes {
                if !write.valid()
                    || write.change.scope != scope
                    || !write.permitted_by(&grants)
                    || write
                        .published_fields()
                        .iter()
                        .any(|field| self.permission_fields.contains(field))
                    || self
                        .app
                        .row_partition(&write.change)
                        .map_err(|error| denied(&error))?
                        != operation.header.extensions.partition
                {
                    return Err(denied("held row/field transition denied"));
                }
                self.app
                    .validate_write(write)
                    .map_err(|error| denied(&error))?;
                stored
                    .rows
                    .push(stored_row(write, self.storage_row(&write.change)?, &names));
            }
            stored.time = stored.time.min(operation.header.timestamp);
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
        if !resolved {
            self.conn
                .execute(
                    "INSERT OR REPLACE INTO secure_held VALUES(?,?)",
                    [id.to_vec(), encode(&stored)?],
                )
                .await?;
        }
        for (frame, _) in frames {
            let operation = frame.operation()?;
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
        let records = rows(&self.conn, "SELECT header,body FROM secure_operations WHERE status IN ('applied','pending') ORDER BY scope,author,sequence").await?;
        for record in records {
            let [SqlValue::Blob(header), SqlValue::Blob(body)] = record.as_slice() else {
                return Err(Error::Corrupt("closed source frame".into()));
            };
            let frame = self.stored_frame(header.clone(), body.clone()).await?;
            let operation = frame.operation()?;
            let extension = &operation.header.extensions;
            if self.stored_status(operation.hash).await?.as_deref() == Some("held") {
                continue;
            }
            if self.frame_reach(
                extension.scope.domain,
                extension.tip,
                extension
                    .author
                    .ok_or_else(|| denied("node author number missing"))?,
                extension.author_sequence,
                operation.header.timestamp,
            )? == tips::FrameReach::Held
                && self.can_read_key(extension.scope, &self.local_grants(time)?)
                && self
                    .opening_key(extension.scope, extension.generation)
                    .is_some()
            {
                self.hold_closed_frame(&frame, time).await?;
            }
        }
        Ok(())
    }
    /// Holds retained writes beyond the common seal before the workspace rebuild.
    pub(in crate::replica) async fn hold_line_writes(
        &self,
        scope: rocky_core::Scope,
        tip: TipId,
        frontier: &[AuthorMark],
    ) -> Result<usize, Error> {
        let records = rows(&self.conn,
            "SELECT header,body FROM secure_operations WHERE status IN ('applied','pending') ORDER BY scope,author,sequence",
        ).await?;
        let mut held = 0;
        for record in records {
            let [SqlValue::Blob(header), SqlValue::Blob(body)] = record.as_slice() else {
                return Err(Error::Corrupt("held line frame".into()));
            };
            let frame = self.stored_frame(header.clone(), body.clone()).await?;
            let operation = frame.operation()?;
            let extension = &operation.header.extensions;
            if extension.scope.domain != scope || extension.tip != tip {
                continue;
            }
            let author = extension
                .author
                .ok_or_else(|| denied("node author number missing"))?;
            let covered = frontier
                .iter()
                .any(|mark| mark.author == author && extension.author_sequence <= mark.sequence);
            if !covered && self.stored_status(operation.hash).await?.as_deref() != Some("held") {
                match self.hold_closed_frame(&frame, now()?).await? {
                    Outcome::Applied => held += 1,
                    Outcome::Waiting => {
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
                let operation = frame.operation()?;
                let extension = &operation.header.extensions;
                inner.verify_frame_authority(frame, now()?)?;
                if inner.frame_reach(
                    extension.scope.domain,
                    extension.tip,
                    extension
                        .author
                        .ok_or_else(|| denied("node author number missing"))?,
                    extension.author_sequence,
                    operation.header.timestamp,
                )? != tips::FrameReach::Held
                {
                    return Err(denied("held transfer names a live or unrelated frame"));
                }
            }
        }
        for status in self.receive_all(frames).await {
            match status? {
                ReceiveStatus::Held
                | ReceiveStatus::Duplicate
                | ReceiveStatus::PendingDependencies => {}
                _ => return Err(denied("held transfer was not held")),
            }
        }
        Ok(())
    }
}
