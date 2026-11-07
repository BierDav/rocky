//! Captured export and final durable-state inspection for live publications.
use super::*;

pub(crate) struct PublicationExport {
    pub upper: Vec<ScopeFrontier>,
    pub pages: Vec<RangePage>,
    pub escrow: Vec<rocky_credentials::VouchedEscrow>,
    pub members: BTreeMap<Hash, Vec<(ResourceScope, PublicKey, u64)>>,
}

impl<A: RowApplication> EncryptedReplica<A> {
    pub(crate) async fn charge_publication_frames(
        &self,
        frames: &[EncryptedFrame],
        used: &mut usize,
    ) -> Result<(), Error> {
        let inner = self.inner.lock(Class::Background).await;
        for frame in frames {
            let extension = &frame.operation_ref()?.header.extensions;
            let Some(key) = inner.opening_key(extension.scope, extension.generation) else {
                // Opaque held bodies allocate no decoded row metadata.
                continue;
            };
            let raw = crate::wire::decrypt(&key.key, &encode(extension)?, frame.body_bytes())?;
            crate::network::publication::charge(&raw, used)?;
            let opened: crate::wire::Opened = decode(&raw)?;
            crate::wire::RowPayload::charge_publication(&opened.payload, used)?;
        }
        Ok(())
    }

    pub(crate) async fn publication_pages(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        cursors: &[ScopeFrontier],
    ) -> Result<PublicationExport, Error> {
        roles::validate_frontiers(cursors)?;
        let (export, commit, flusher) = {
            let mut inner = self.inner.lock(Class::Background).await;
            let mut upper = Vec::new();
            for cursor in cursors {
                cursor.validate()?;
                inner.authorize_peer(peer, credentials, cursor.scope)?;
                let frontier = inner.frontier(cursor.scope).await?;
                if frontier != *cursor {
                    upper.push(frontier);
                }
            }
            let mut pages = Vec::new();
            let mut members = BTreeMap::new();
            let mut weight = 0usize;
            for fence in &upper {
                let cursor = cursors
                    .iter()
                    .find(|c| c.scope == fence.scope)
                    .ok_or_else(|| denied("publication cursor"))?;
                let mut after = None;
                loop {
                    let page = inner
                        .range_frames(
                            peer,
                            credentials,
                            cursor,
                            after,
                            crate::network::MAX_PAGE_BYTES,
                            Some(fence),
                        )
                        .await?;
                    if page.behind {
                        return Err(denied("publication compacted cursor"));
                    }
                    for frame in &page.frames {
                        weight = weight.saturating_add(frame.weight());
                        let required = inner.publication_members(frame).await?;
                        for &(scope, author, sequence) in &required {
                            // An unauthorized sibling is not a recipient requirement.
                            if inner.authorize_peer(peer, credentials, scope).is_err() {
                                continue;
                            }
                            let covered = cursors.iter().any(|c| {
                                c.scope == scope
                                    && (c.log(author).is_some_and(|l| l.held > sequence)
                                        || upper.iter().any(|f| {
                                            f.scope == scope
                                                && f.log(author).is_some_and(|l| l.held > sequence)
                                        }))
                            });
                            if !covered {
                                return Err(denied("publication incomplete bundle"));
                            }
                        }
                        members.insert(frame.id()?, required);
                    }
                    weight = page
                        .voids
                        .iter()
                        .fold(weight, |n, v| n.saturating_add(v.weight()));
                    if weight > crate::network::publication::MAX_BYTES {
                        return Err(denied("publication byte quota"));
                    }
                    after = page.next;
                    pages.push(page);
                    if after.is_none() {
                        break;
                    }
                }
            }
            // Exact log coverage catches staged records and unavailable gaps.
            for fence in &upper {
                let cursor = cursors
                    .iter()
                    .find(|c| c.scope == fence.scope)
                    .ok_or_else(|| denied("publication cursor"))?;
                let mut positions = BTreeMap::<PublicKey, BTreeSet<u64>>::new();
                for page in pages.iter().filter(|p| p.scope == fence.scope) {
                    for frame in &page.frames {
                        let h = &frame.operation_ref()?.header;
                        positions.entry(h.public_key).or_default().insert(h.seq_num);
                    }
                    for record in &page.voids {
                        for (_, h, _) in record.verify()? {
                            if h.extensions.scope == fence.scope {
                                positions.entry(h.public_key).or_default().insert(h.seq_num);
                            }
                        }
                    }
                }
                for log in &fence.logs {
                    let mut next = cursor.log(log.author).map_or(0, |l| l.held);
                    for n in positions.get(&log.author).into_iter().flatten() {
                        if *n < next {
                            continue;
                        }
                        if *n != next || *n >= log.held {
                            return Err(denied("publication incomplete range"));
                        }
                        next = next
                            .checked_add(1)
                            .ok_or_else(|| denied("publication position exhausted"))?;
                    }
                    if next < log.held {
                        return Err(denied("publication incomplete range"));
                    }
                }
            }
            let slots = pages
                .iter()
                .flat_map(|page| &page.frames)
                .map(|frame| Ok((frame.scope()?, frame.generation()?)))
                .collect::<Result<BTreeSet<_>, Error>>()?;
            let escrow = inner
                .escrow_of(&slots, Some(crate::network::publication::MAX_BYTES))
                .await?;
            (
                PublicationExport {
                    upper,
                    pages,
                    escrow,
                    members,
                },
                inner.commit,
                inner.flusher.clone(),
            )
        };
        flusher.run(commit).await?;
        Ok(export)
    }

    pub(crate) async fn offer_publication(&self, ids: &[Hash]) -> Result<(), Error> {
        let (commit, flusher) = {
            let mut inner = self.inner.lock(Class::Background).await;
            for id in ids {
                let mut record = inner
                    .conn
                    .query(
                        "SELECT 1 FROM secure_operations WHERE hash=? AND status<>'rejected'",
                        [id.as_bytes().to_vec()],
                    )
                    .await?;
                if record.next().await?.is_none() {
                    return Err(denied("publication changed during preflight"));
                }
            }
            inner.offer(ids).await?;
            (inner.commit, inner.flusher.clone())
        };
        flusher.run(commit).await
    }

    pub(crate) async fn publication_state(
        &self,
        expected: &[ScopeFrontier],
        received: &[(ResourceScope, Hash)],
    ) -> Result<crate::network::PublicationReceipt, Error> {
        self.settle().await?;
        let inner = self.inner.lock(Class::Background).await;
        use crate::network::PublicationRepairReason;
        let mut repair = BTreeSet::new();
        let mut repair_all = false;
        let mut reasons = BTreeSet::new();
        let mut inspect: BTreeMap<_, _> =
            received.iter().map(|(scope, id)| (*id, *scope)).collect();
        // Earlier held receipts can leave unresolved records below this publication's
        // cursors. A terminal with no new frames must still report those blockers.
        let mut pending = 0;
        let grants = inner.local_grants(now()?)?;
        for fence in expected {
            if !inner.can_read_key(fence.scope, &grants) {
                continue;
            }
            let current = inner.frontier(fence.scope).await?;
            for log in &fence.logs {
                let applied = current.log(log.author).map_or(0, |log| log.applied);
                if applied >= log.held {
                    continue;
                }
                let mut records = inner.conn.query(
                    "SELECT hash FROM secure_operations WHERE scope=? AND author=? AND sequence>=? AND sequence<? AND status IN ('pending','rejected') ORDER BY sequence LIMIT 65",
                    bindings(vec![
                        SqlValue::Blob(encode(&fence.scope)?),
                        SqlValue::Blob(log.author.as_bytes().to_vec()),
                        SqlValue::Integer(i64::try_from(applied).map_err(|_| denied("publication position"))?),
                        SqlValue::Integer(i64::try_from(log.held).map_err(|_| denied("publication position"))?),
                    ]),
                ).await?;
                while let Some(row) = records.next().await? {
                    pending += 1;
                    if pending > 64 {
                        repair_all = true;
                        reasons.insert(PublicationRepairReason::UnknownRequirements);
                        break;
                    }
                    inspect.insert(checkpoint::hash_of(&row.get::<Vec<u8>>(0)?)?, fence.scope);
                }
                if pending > 64 {
                    break;
                }
            }
            if pending > 64 {
                break;
            }
        }
        for (id, scope) in inspect {
            let status = inner.status(id).await?;
            match status {
                ReceiveStatus::PendingDependencies
                | ReceiveStatus::MissingKey
                | ReceiveStatus::Rejected => {
                    reasons.insert(match status {
                        ReceiveStatus::MissingKey => PublicationRepairReason::MissingKey,
                        ReceiveStatus::Rejected => PublicationRepairReason::Rejected,
                        _ => PublicationRepairReason::PendingDependencies,
                    });
                    repair.insert(scope);
                    if let Some(frame) = inner.frame(id).await? {
                        // A missing key has no readable member openings yet.
                        if let Ok(members) = inner.publication_members(&frame).await {
                            repair.extend(members.into_iter().map(|(scope, _, _)| scope));
                        } else {
                            repair_all = true;
                            reasons.insert(PublicationRepairReason::UnknownRequirements);
                        }
                    }
                }
                _ => {}
            }
        }
        let mut frontiers = Vec::new();
        for fence in expected {
            let current = inner.frontier(fence.scope).await?;
            let mut acknowledged = fence.clone();
            for log in &mut acknowledged.logs {
                let held = current.log(log.author);
                log.held = log.held.min(held.map_or(0, |l| l.held));
                log.applied = log.held.min(held.map_or(0, |l| l.applied));
            }
            if acknowledged
                .logs
                .iter()
                .zip(&fence.logs)
                .any(|(a, b)| a.held < b.held)
            {
                repair.insert(fence.scope);
                reasons.insert(PublicationRepairReason::MissingHistory);
            }
            frontiers.push(acknowledged);
        }
        Ok(crate::network::PublicationReceipt {
            frontiers,
            repair: repair.into_iter().collect(),
            repair_all,
            reasons: reasons.into_iter().collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{Fixture, live_publication::pair, revocations};
    use rocky_platform::fs::Scratch;

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn publication_upper_fences_exclude_later_positions_and_new_author_logs() {
        let f = Fixture::new();
        let dir = Scratch::new().await;
        let (a, b, _, bob) = pair(&f, &dir).await;
        let cursor = b.frontier(f.public).await.unwrap();
        let first = revocations::insert(&a, &f, 1, "captured").await;
        let fence = a.frontier(f.public).await.unwrap();
        revocations::insert(&a, &f, 2, "later").await;
        let carol = PrivateKey::new();
        let c = revocations::open(&f, &dir, "c.db", &carol).await;
        a.receive(revocations::insert(&c, &f, 3, "new author").await)
            .await
            .unwrap();
        let credentials = f.credentials(&bob, f.full());
        a.admit_peer(bob.public_key(), &credentials, b.followed_tip().await)
            .await
            .unwrap();
        let page = a
            .inner
            .lock(Class::Background)
            .await
            .range_frames(
                bob.public_key(),
                &credentials,
                &cursor,
                None,
                crate::network::MAX_PAGE_BYTES,
                Some(&fence),
            )
            .await
            .unwrap();
        assert_eq!(page.frames.len(), 1);
        assert_eq!(page.frames[0].id().unwrap(), first.id().unwrap());
        assert_eq!(page.frontier, fence);
        assert!(page.next.is_none());
        assert_ne!(a.frontier(f.public).await.unwrap(), fence);
        let next = a
            .publication_pages(bob.public_key(), &credentials, &[fence])
            .await
            .unwrap();
        assert_eq!(next.pages.iter().map(|p| p.frames.len()).sum::<usize>(), 2);
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn publication_rechecks_retired_frames_before_marking_an_export_offered() {
        let f = Fixture::new();
        let dir = Scratch::new().await;
        let (a, b, _, bob) = pair(&f, &dir).await;
        let cursor = b.frontier(f.public).await.unwrap();
        let frame = revocations::insert(&a, &f, 1, "captured").await;
        let id = frame.id().unwrap();
        let credentials = f.credentials(&bob, f.full());
        a.admit_peer(bob.public_key(), &credentials, b.followed_tip().await)
            .await
            .unwrap();
        let export = a
            .publication_pages(bob.public_key(), &credentials, &[cursor])
            .await
            .unwrap();
        assert_eq!(export.pages[0].frames[0].id().unwrap(), id);
        // A deterministic preflight/offer gate: retirement may run after export ownership ends.
        a.inner
            .lock(Class::Background)
            .await
            .reject(id)
            .await
            .unwrap();
        assert!(a.offer_publication(&[id]).await.is_err());
        let inner = a.inner.lock(Class::Background).await;
        let mut record = inner
            .conn
            .query(
                "SELECT 1 FROM secure_unoffered WHERE hash=?",
                [id.as_bytes().to_vec()],
            )
            .await
            .unwrap();
        assert!(record.next().await.unwrap().is_some());
    }
}
