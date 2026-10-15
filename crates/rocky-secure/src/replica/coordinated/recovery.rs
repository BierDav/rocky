//! Recovery evidence from ordinary member replicas.
use super::*;

impl<A: RowApplication> Inner<A> {
    /// Members recover entry evidence from complete retained bundles. The
    /// requesting trusted node checks the hashes against its coordinator.
    pub(super) async fn recovered_entries(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        log: TipLog,
        after: u64,
    ) -> Result<Vec<CommittedEntry>, Error> {
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
                let operation = frame.operation()?;
                if operation.header.extensions.tip != log.tip {
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
                let Some(position) = membership.entry.filter(|position| *position > after) else {
                    continue;
                };
                self.authorize_peer(peer, credentials, frame.scope()?)?;
                let frames = match self.members_of(&frame).await {
                    Ok(Ok(frames)) => frames,
                    Ok(Err(_)) => continue,
                    Err(Error::Rejected(reason)) if reason == "bundle member key unavailable" => {
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                for (index, member) in frames.iter().enumerate() {
                    self.authorize_peer(peer, credentials, member.scope()?)?;
                    if staged.contains(&member.id()?)
                        || self.stored_status(member.id()?).await?.as_deref() != Some("applied")
                    {
                        return Err(denied("recovery bundle is not fully applied"));
                    }
                    let member_payload = self.row_payload(member)?;
                    let Some(other) = member_payload.membership() else {
                        return Err(denied("recovery bundle member is missing"));
                    };
                    if other.transaction != membership.transaction
                        || other.members != membership.members
                        || other.entry != Some(position)
                        || usize::from(other.index) != index
                        || member.scope()?.domain != log.scope
                        || member.operation()?.header.extensions.tip != log.tip
                    {
                        return Err(denied("recovery bundle members disagree"));
                    }
                }
                let ids = ids_of(&frames)?;
                let hash = entry_hash(log, position, &ids)?;
                candidates
                    .entry((position, hash))
                    .or_insert(CommittedEntry {
                        position,
                        hash,
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
