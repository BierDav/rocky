//! Recovery evidence from ordinary member replicas.
use super::*;

impl<A: RowApplication> Inner<A> {
    /// Evidence in retained frames and undecided durable storage. A receiver
    /// verifies the whole chain against its coordinator before applying it.
    pub(super) async fn collectible_entries(
        &self,
        peer: Option<(PublicKey, &CredentialChain)>,
        log: TipLog,
        after: u64,
    ) -> Result<Vec<CommittedEntry>, Error> {
        let mut found: BTreeMap<u64, CommittedEntry> = self
            .recovered_entries(peer, log, after)
            .await?
            .into_iter()
            .map(|entry| (entry.position, entry))
            .collect();
        for entry in self
            .entries(log, &["staged", "assigned", "stored"])
            .await?
            .into_iter()
            .filter(|entry| entry.position > after)
        {
            let (frames, voids) = if entry.state == "stored" {
                decode_bounded::<(Vec<EncryptedFrame>, Vec<SignedVoid>)>(
                    &entry.frames,
                    MAX_SECURE_BYTES,
                )?
            } else {
                let ids = decode_bounded::<Vec<Hash>>(&entry.frames, MAX_SECURE_BYTES)?;
                let frames = self.own_frames(&ids).await?;
                let voids = self.voids_before(&frames).await?;
                (frames, voids)
            };
            let ids = ids_of(&frames)?;
            if let Some((peer, credentials)) = peer {
                for frame in &frames {
                    self.authorize_log_peer(peer, credentials, frame.scope()?, log)?;
                }
                if frames
                    .iter()
                    .any(|frame| self.authorize_log_frame(peer, log, frame).is_err())
                {
                    continue;
                }
            }
            if let Some(held) = found.get(&entry.position)
                && held.hash != entry.hash
            {
                return Err(denied(
                    "invalid committed evidence: conflicting stored entries",
                ));
            }
            found.entry(entry.position).or_insert(CommittedEntry {
                position: entry.position,
                hash: entry.hash,
                carry: entry.carry,
                ids,
                before: None,
                frames,
                voids,
            });
        }
        Ok(found.into_values().take(MAX_CATCH_UP_ENTRIES).collect())
    }
    /// Members recover entry evidence from complete retained bundles. The
    /// requesting trusted node checks the hashes against its coordinator;
    /// without a `peer` this node collects what it holds itself.
    pub(super) async fn recovered_entries(
        &self,
        peer: Option<(PublicKey, &CredentialChain)>,
        log: TipLog,
        after: u64,
    ) -> Result<Vec<CommittedEntry>, Error> {
        let authorize = |scope| match peer {
            Some((peer, credentials)) => self.authorize_log_peer(peer, credentials, scope, log),
            None => Ok(()),
        };
        let staged = self.staged().await?;
        let mut candidates = BTreeMap::new();
        let mut cursor = Vec::new();
        loop {
            let page = collect_rows(self.conn.query(
                "SELECT hash,header,body FROM secure_operations WHERE status='applied' AND hash>? ORDER BY hash LIMIT 256",
                [cursor.clone()],
            ).await?).await?;
            let complete = page.len() < 256;
            for record in page {
                let [
                    SqlValue::Blob(hash),
                    SqlValue::Blob(header),
                    SqlValue::Blob(body),
                ] = record.as_slice()
                else {
                    return Err(Error::Corrupt("recovery frame".into()));
                };
                cursor = hash.clone();
                let frame = self.stored_frame(header.clone(), body.clone()).await?;
                if frame.scope()?.domain != log.scope || staged.contains(&frame.id()?) {
                    continue;
                }
                let operation = frame.operation_ref()?;
                // A carried write's frames are placed by their carry record;
                // the log they sign no longer holds them.
                if operation.header.extensions.tip != log.tip
                    && self
                        .carry_of(
                            frame.id()?,
                            operation.header.public_key,
                            log.scope,
                            operation.header.extensions.tip,
                        )?
                        .is_none_or(|carry| carry.statement.tip != log.tip)
                {
                    continue;
                }
                if self
                    .opening_key(frame.scope()?, operation.header.extensions.generation)
                    .is_none()
                {
                    continue;
                }
                let payload = self.row_payload(&frame)?;
                let Some(membership) = payload.membership() else {
                    continue;
                };
                let Some((at, position)) = self.coordinate(&frame)? else {
                    continue;
                };
                if at != log || position <= after {
                    continue;
                }
                authorize(frame.scope()?)?;
                if let Some((peer, _)) = peer
                    && self.authorize_log_frame(peer, log, &frame).is_err()
                {
                    continue;
                }
                let frames = match self.members_of(&frame).await {
                    Ok(Ok(frames)) => frames,
                    Ok(Err(_)) => continue,
                    Err(Error::Rejected(reason)) if reason == "bundle member key unavailable" => {
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                if let Some((peer, _)) = peer
                    && frames
                        .iter()
                        .any(|frame| self.authorize_log_frame(peer, log, frame).is_err())
                {
                    continue;
                }
                let mut agree = true;
                for (index, member) in frames.iter().enumerate() {
                    authorize(member.scope()?)?;
                    if staged.contains(&member.id()?)
                        || self.stored_status(member.id()?).await?.as_deref() != Some("applied")
                    {
                        return Err(denied("recovery bundle is not fully applied"));
                    }
                    let member_payload = self.row_payload(member)?;
                    let Some(other) = member_payload.membership() else {
                        return Err(denied("recovery bundle member is missing"));
                    };
                    // Members agree on the signed coordinate and on where
                    // it resolves; a mismatching candidate is no evidence.
                    let member_operation = member.operation_ref()?;
                    if other.transaction != membership.transaction
                        || other.members != membership.members
                        || other.entry != membership.entry
                        || usize::from(other.index) != index
                        || member.scope()?.domain != log.scope
                        || member_operation.header.extensions.tip != operation.header.extensions.tip
                        || member_operation.header.public_key != operation.header.public_key
                        || self.coordinate(member)? != Some((log, position))
                    {
                        agree = false;
                        break;
                    }
                }
                if !agree {
                    continue;
                }
                let ids = ids_of(&frames)?;
                let hash = entry_hash(log, position, &ids)?;
                let carry = self.entry_carry(log, position, &ids);
                candidates
                    .entry((position, hash))
                    .or_insert(CommittedEntry {
                        position,
                        hash,
                        carry,
                        ids,
                        before: None,
                        voids: self.voids_before(&frames).await?,
                        frames,
                    });
                if candidates.len() > MAX_CATCH_UP_ENTRIES {
                    candidates.pop_last();
                }
            }
            if complete {
                break;
            }
        }
        Ok(candidates
            .into_values()
            .take(MAX_CATCH_UP_ENTRIES)
            .collect())
    }
}
