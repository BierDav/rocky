use super::*;

impl<A: RowApplication> EncryptedReplica<A> {
    /// The whole scope, within `MAX_SNAPSHOT_ROWS` and `MAX_SNAPSHOT_BYTES`.
    /// Larger scopes are read with `snapshot_page`.
    pub async fn snapshot(&self, scope: ResourceScope) -> Result<SecureSnapshot, Error> {
        let inner = self.inner.lock().await;
        let page = inner
            .page(scope, None, MAX_SNAPSHOT_ROWS, MAX_SNAPSHOT_BYTES)
            .await?;
        if page.next.is_some() {
            return Err(Error::Quota {
                resource: "encrypted snapshot",
                limit: MAX_SNAPSHOT_ROWS,
            });
        }
        Ok(SecureSnapshot {
            revision: page.revision,
            rows: page.rows,
            complete: page.complete,
        })
    }
    /// Reads live rows after `after` in row order: at most `max_rows` rows and
    /// `MAX_SNAPSHOT_BYTES`, with `next` set when more remain. Every page checks
    /// read authority and reports its own revision.
    pub async fn snapshot_page(
        &self,
        scope: ResourceScope,
        after: Option<Commitment>,
        max_rows: usize,
    ) -> Result<SecurePage, Error> {
        if max_rows == 0 || max_rows > MAX_SNAPSHOT_ROWS {
            return Err(Error::Quota {
                resource: "encrypted snapshot",
                limit: MAX_SNAPSHOT_ROWS,
            });
        }
        self.inner
            .lock()
            .await
            .page(scope, after, max_rows, MAX_SNAPSHOT_BYTES)
            .await
    }
    pub async fn companion(
        &self,
        scope: ResourceScope,
        row: Commitment,
    ) -> Result<Companion, Error> {
        self.inner.lock().await.companion(scope, row).await
    }
    /// Forward original bytes only. Restart a pass after subscription changes or new insertions.
    pub async fn transfer_page(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scopes: &[ResourceScope],
        after: Option<Hash>,
        byte_quota: usize,
    ) -> Result<CiphertextPage, Error> {
        // Frames leave the device: flush every commit they may include,
        // first without the replica so local statements do not wait for it.
        self.flush().await?;
        let mut inner = self.inner.lock().await;
        inner.flush()?;
        let grants = inner.verify_chain(credentials, peer, now()?)?;
        inner.same_tip_peer(peer)?;
        if scopes.is_empty()
            || scopes.len() > 64
            || byte_quota == 0
            || byte_quota > MAX_SECURE_FRAME
        {
            return Err(denied("sync quota"));
        }
        for scope in scopes {
            if !grants.iter().any(|grant| {
                grant.scope == *scope
                    && grant.rows == RowSelection::All
                    && (grant.rights.contains(&Right::Custody)
                        || grant.rights.contains(&Right::Read))
            }) {
                return Err(denied("ciphertext custody denied"));
            }
        }
        // Never a silent gap: every page names the scopes with pruned history.
        let mut pruned = Vec::new();
        for scope in scopes {
            if inner.has_pruned_prefix(*scope).await? {
                pruned.push(*scope);
            }
        }
        let placeholders = vec!["?"; scopes.len()].join(",");
        let mut values = scopes
            .iter()
            .map(|scope| encode(scope).map(SqlValue::Blob))
            .collect::<Result<Vec<_>, _>>()?;
        values.push(SqlValue::Blob(
            after
                .map(|hash| hash.as_bytes().to_vec())
                .unwrap_or_default(),
        ));
        let records = collect_rows(inner.conn.query(
            format!("SELECT hash,header,body FROM secure_operations WHERE scope IN ({placeholders}) AND hash>? ORDER BY hash LIMIT 33"),
            bindings(values),
        ).await?).await?;
        let limited = records.len() == 33;
        let staged = inner.staged().await?;
        let mut frames = Vec::new();
        let mut used = 0;
        let mut last = None;
        for record in records {
            let [
                SqlValue::Blob(id),
                SqlValue::Blob(header),
                SqlValue::Blob(body),
            ] = record.as_slice()
            else {
                return Err(denied("corrupt operation"));
            };
            let id = Hash::try_from(id.as_slice()).map_err(|_| denied("invalid operation hash"))?;
            let frame = inner.stored_frame(header.clone(), body.clone()).await?;
            if !scopes.contains(&frame.scope()?) {
                continue;
            }
            if staged.contains(&id) {
                last = Some(id);
                continue;
            }
            if !inner.visible_in_workspace(&frame.operation()?)? {
                last = Some(id);
                continue;
            }
            let weight = frame.weight();
            if frames.len() == 32 || used + weight > byte_quota {
                if frames.is_empty() {
                    return Err(denied("byte quota smaller than one frame"));
                }
                return inner.offered_page(frames, last, pruned).await;
            }
            used += weight;
            last = Some(id);
            frames.push(frame);
        }
        // Skipped frames can leave a full query short of a full page.
        let next = if limited { last } else { None };
        inner.offered_page(frames, next, pruned).await
    }
    pub async fn acknowledge(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        id: Hash,
        applied: bool,
    ) -> Result<(), Error> {
        let mut inner = self.inner.lock().await;
        let grants = inner.verify_chain(credentials, peer, now()?)?;
        let Some(frame) = inner.frame(id).await? else {
            // Pruned since the peer's page: there is nothing left to record.
            return Ok(());
        };
        let scope = frame.scope()?;
        let right = if applied { Right::Read } else { Right::Custody };
        if !grants.iter().any(|grant| {
            grant.scope == scope
                && grant.rows == RowSelection::All
                && (grant.rights.contains(&right)
                    || (!applied && grant.rights.contains(&Right::Read)))
        }) {
            return Err(denied("acknowledgment right denied"));
        }
        transaction!(@cache_neutral inner, |inner| {
            inner.conn.execute("INSERT INTO secure_ack VALUES(?,?,?) ON CONFLICT(peer,hash) DO UPDATE SET applied=MAX(applied,excluded.applied)", bindings(vec![SqlValue::Blob(rocky_credentials::PeerId::of(peer).as_bytes().to_vec()), SqlValue::Blob(id.as_bytes().to_vec()), SqlValue::Integer(i64::from(applied))])).await?;
            Ok(())
        })?;
        // The peer is told its acknowledgment is recorded.
        inner.flush()
    }
}

impl<A: RowApplication> Inner<A> {
    pub(super) async fn companion(
        &self,
        scope: ResourceScope,
        row: Commitment,
    ) -> Result<Companion, Error> {
        self.require_scope_readable(scope).await?;
        let has_key = self.readable(scope)?;
        // An evicted scope is unavailable, never deleted.
        let record = if has_key && self.holding(scope).await? != Some(roles::Holding::Evicting) {
            self.key_row(scope, row).await?
        } else {
            None
        };
        // An unfinished import never proves a deletion.
        match record {
            Some(record) if !record.deleted => Ok(Companion::Present(record.fields)),
            Some(_) if !self.has_pending(scope).await? && !self.importing(scope).await? => {
                Ok(Companion::Deleted)
            }
            _ => Ok(Companion::Unavailable),
        }
    }
    /// Checks whole-row read authority; returns whether the scope's key is installed.
    pub(super) fn readable(&self, scope: ResourceScope) -> Result<bool, Error> {
        if !self.can_read_key(scope, &self.local_grants(now()?)?) {
            return Err(denied("whole-row read denied"));
        }
        Ok(self.config.keys.iter().any(|key| key.scope == scope))
    }
    pub(super) async fn has_pending(&self, scope: ResourceScope) -> Result<bool, Error> {
        let mut pending = self
            .conn
            .query(
                "SELECT 1 FROM secure_operations WHERE scope=? AND status='pending' LIMIT 1",
                [encode(&scope)?],
            )
            .await?;
        Ok(pending.next().await?.is_some())
    }
    pub(super) async fn page(
        &self,
        scope: ResourceScope,
        after: Option<Commitment>,
        max_rows: usize,
        max_bytes: usize,
    ) -> Result<SecurePage, Error> {
        self.require_scope_readable(scope).await?;
        let has_key = self.readable(scope)?;
        // Rows of a scope that no longer follows the peers, or whose
        // import is unfinished, are not complete.
        let holding = self.holding(scope).await?;
        let mut page = SecurePage {
            revision: self.revision,
            rows: BTreeMap::new(),
            complete: has_key
                && self.holds(scope).await?
                && !self.has_pending(scope).await?
                && !self.importing(scope).await?,
            next: None,
        };
        if !has_key || holding == Some(roles::Holding::Evicting) {
            return Ok(page);
        }
        let resource = scope;
        let scope = encode(&scope)?;
        let mut last = after;
        let mut used = 0usize;
        loop {
            // Tombstones are skipped, so a page may need several chunks.
            let chunk = collect_rows(
                self.conn
                    .query(
                        "SELECT row,fields FROM secure_rows WHERE scope=? AND row>? ORDER BY row LIMIT ?",
                        bindings(vec![
                            SqlValue::Blob(scope.clone()),
                            SqlValue::Blob(last.map(|row| row.to_vec()).unwrap_or_default()),
                            SqlValue::Integer(PAGE_CHUNK as i64),
                        ]),
                    )
                    .await?,
            )
            .await?;
            let exhausted = chunk.len() < PAGE_CHUNK;
            for row in chunk {
                let [SqlValue::Blob(id), SqlValue::Blob(bytes)] = row.as_slice() else {
                    return Err(denied("corrupt row"));
                };
                let id: Commitment = id
                    .as_slice()
                    .try_into()
                    .map_err(|_| denied("corrupt row identity"))?;
                let record: Materialized = decode(bytes)?;
                if !record.deleted
                    && (record.identity.is_none()
                        || self
                            .winner(
                                resource,
                                record.key.ok_or_else(|| denied("missing row key"))?,
                            )
                            .await?
                            .is_some_and(|winner| winner.identity == record.identity))
                {
                    let full =
                        page.rows.len() == max_rows || used.saturating_add(bytes.len()) > max_bytes;
                    if full && !page.rows.is_empty() {
                        page.next = last;
                        return Ok(page);
                    }
                    used += bytes.len();
                    page.rows.insert(record.key.unwrap_or(id), record.fields);
                }
                last = Some(id);
            }
            if exhausted {
                return Ok(page);
            }
        }
    }
}

impl<A: RowApplication> Inner<A> {
    /// A transfer page with the voids its frames continue after, its
    /// frames marked offered before they leave.
    async fn offered_page(
        &mut self,
        frames: Vec<EncryptedFrame>,
        next: Option<Hash>,
        pruned: Vec<ResourceScope>,
    ) -> Result<CiphertextPage, Error> {
        let hashes = frames
            .iter()
            .map(EncryptedFrame::id)
            .collect::<Result<Vec<_>, _>>()?;
        self.offer(&hashes).await?;
        Ok(CiphertextPage {
            voids: self.voids_before(&frames).await?,
            frames,
            next,
            pruned,
        })
    }
}
