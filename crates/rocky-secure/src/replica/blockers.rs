//! Blockers derived from stored state. The read, write and import gates
//! answer from these predicates, and diagnostics read the same lists.
use super::*;
use rocky_core::{Scope, domain_label, scope_label};

fn blocker(scope: String, kind: BlockerKind, clears_by: ClearsBy) -> Blocker {
    Blocker {
        scope,
        kind,
        clears_by,
        since: None,
    }
}
/// Refuses with every blocker, if any.
pub(super) fn refuse(blockers: Vec<Blocker>) -> Result<(), Error> {
    if blockers.is_empty() {
        Ok(())
    } else {
        Err(Error::Blocked(blockers))
    }
}

impl<A: RowApplication> Inner<A> {
    /// What holds back authoring anywhere in `domain`: an import outside a
    /// seal, a lost compaction, and every scope whose base must move to a
    /// seal of the followed path.
    pub(super) async fn workspace_blockers(&self, domain: Scope) -> Result<Vec<Blocker>, Error> {
        let mut blockers = Vec::new();
        if self.graphs.contains_key(&domain) {
            // An evicted scope reads as unavailable; its own log returning
            // holds back only authoring.
            for (scope, id) in self.domain_scopes(domain).await? {
                if let Some(staging) = self.staging(id).await?
                    && staging.signed.statement.seal.is_none()
                {
                    blockers.push(blocker(
                        scope_label(&scope),
                        BlockerKind::StagedImport {
                            sealed: false,
                            pages_complete: staging.complete,
                        },
                        ClearsBy::AnyPeer,
                    ));
                }
            }
        }
        let needs = self.seal_rebootstraps(domain).await?;
        if self.compaction_lost(domain).await?.is_some() {
            blockers.push(blocker(
                domain_label(&domain),
                BlockerKind::LostCompaction,
                if needs.is_empty() {
                    ClearsBy::LocalWork
                } else {
                    ClearsBy::AnyPeer
                },
            ));
        }
        for need in needs {
            blockers.push(blocker(
                scope_label(&need.scope),
                BlockerKind::SealRebootstrap {
                    replay: need.replay,
                    listed: need.listed.len() as u64,
                },
                match need.replay {
                    FrameReplay::Local => ClearsBy::LocalWork,
                    FrameReplay::Peers | FrameReplay::Unavailable => ClearsBy::AnyPeer,
                },
            ));
        }
        Ok(blockers)
    }
    /// What holds back reading `scope`: the workspace-wide blockers, then
    /// only this scope's own state. A scope another scope's rebootstrap
    /// waits for stays readable; one that waits itself reads only the
    /// verified pages of a direct import.
    pub(super) async fn readable_blockers(
        &self,
        scope: ResourceScope,
    ) -> Result<Vec<Blocker>, Error> {
        let label = scope_label(&scope);
        let mut blockers = self.workspace_blockers(scope.domain).await?;
        let mut direct = None;
        let mut kept = Vec::with_capacity(blockers.len());
        for blocker in blockers.drain(..) {
            if matches!(blocker.kind, BlockerKind::SealRebootstrap { .. }) {
                if blocker.scope != label {
                    continue;
                }
                if *direct.get_or_insert(self.authenticated_direct_import(scope).await?) {
                    continue;
                }
            }
            kept.push(blocker);
        }
        Ok(kept)
    }
    /// Frames of a dropped tip on the followed tip's ancestry that are not
    /// back yet: authoring waits for them.
    pub(super) async fn dropped_tip_blockers(&self) -> Result<Vec<Blocker>, Error> {
        let graph = self
            .graphs
            .iter()
            .find(|(_, graph)| graph.schema_any(self.workspace.tip).is_ok());
        let mut count = 0u64;
        let mut missing = self.conn.query(
            "SELECT tip FROM secure_dropped_tip_frames d WHERE NOT EXISTS(SELECT 1 FROM secure_operations o WHERE o.hash=d.hash)", ()).await?;
        while let Some(row) = missing.next().await? {
            let dropped: TipId = decode(&row.get::<Vec<u8>>(0)?)?;
            let mut cursor = self.workspace.tip;
            let mut visited = BTreeSet::new();
            while visited.insert(cursor) {
                if dropped == cursor {
                    count += 1;
                    break;
                }
                let Some((_, graph)) = graph else { break };
                let Some((parent, _)) = Self::tip_parent(graph, cursor)? else {
                    break;
                };
                cursor = parent;
            }
        }
        if count == 0 {
            return Ok(Vec::new());
        }
        let label = match (graph, &self.workspace.typed) {
            (Some((domain, _)), _) => domain_label(domain),
            (None, Some(typed)) => domain_label(&typed.domain),
            (None, None) => String::new(),
        };
        Ok(vec![blocker(
            label,
            BlockerKind::DroppedTipFrames { count },
            ClearsBy::AnyPeer,
        )])
    }
    /// A device that may write authors with a node of its identity for the
    /// followed line; until one is registered nothing is written.
    pub(super) fn line_node_blockers(&self) -> Result<Vec<Blocker>, Error> {
        if self.node_author.is_some() {
            return Ok(Vec::new());
        }
        let writes = self.local_grants(now()?)?.iter().any(|grant| {
            [Right::Insert, Right::Update, Right::Delete]
                .iter()
                .any(|right| grant.rights.contains(right))
        });
        let domain = match (&self.workspace.followed, &self.workspace.typed) {
            (Some((domain, _)), _) => *domain,
            (None, Some(typed)) => typed.domain,
            (None, None) => return Ok(Vec::new()),
        };
        Ok(if writes {
            vec![blocker(
                domain_label(&domain),
                BlockerKind::LineNode,
                ClearsBy::LocalWork,
            )]
        } else {
            Vec::new()
        })
    }
    /// Pending frames by scope and the reason their log heads wait.
    async fn pending_blockers(&self) -> Result<Vec<Blocker>, Error> {
        let mut counts: BTreeMap<(String, WaitReason), u64> = BTreeMap::new();
        for record in rows(
            &self.conn,
            "SELECT o.scope,o.hash,h.n FROM secure_operations o JOIN (SELECT scope,author,min(sequence) AS sequence,count(*) AS n FROM secure_operations WHERE status='pending' GROUP BY scope,author) h ON o.author=h.author AND o.scope=h.scope AND o.sequence=h.sequence",
        )
        .await?
        {
            let [
                SqlValue::Blob(scope),
                SqlValue::Blob(hash),
                SqlValue::Integer(count),
            ] = record.as_slice()
            else {
                return Err(Error::Corrupt("pending operation".into()));
            };
            let scope: ResourceScope = decode(scope)?;
            let hash = Hash::from_bytes(
                hash.as_slice()
                    .try_into()
                    .map_err(|_| Error::Corrupt("pending operation".into()))?,
            );
            let reason = self
                .waits
                .get(&hash)
                .copied()
                .unwrap_or(WaitReason::Unexamined);
            *counts.entry((scope_label(&scope), reason)).or_default() += *count as u64;
        }
        Ok(counts
            .into_iter()
            .map(|((scope, reason), count)| {
                blocker(
                    scope,
                    BlockerKind::PendingFrames { reason, count },
                    reason.clears_by(),
                )
            })
            .collect())
    }
    /// Imports in progress and eviction guards, scope by scope.
    async fn scope_state_blockers(&self) -> Result<Vec<Blocker>, Error> {
        let mut blockers = Vec::new();
        let mut staged = BTreeSet::new();
        for record in rows(
            &self.conn,
            "SELECT s.scope,r.signed,r.complete FROM secure_rebootstrap r JOIN secure_scope_state s ON s.id=r.scope",
        )
        .await?
        {
            let [
                SqlValue::Blob(scope),
                SqlValue::Blob(signed),
                SqlValue::Integer(complete),
            ] = record.as_slice()
            else {
                return Err(Error::Corrupt("staged import".into()));
            };
            let scope: ResourceScope = decode(scope)?;
            staged.insert(scope);
            blockers.push(blocker(
                scope_label(&scope),
                BlockerKind::StagedImport {
                    sealed: SignedCheckpoint::from_bytes(signed)?
                        .statement
                        .seal
                        .is_some(),
                    pages_complete: *complete == 1,
                },
                ClearsBy::AnyPeer,
            ));
        }
        for scope in &self.importing {
            if !staged.contains(scope) && self.importing(*scope).await? {
                blockers.push(blocker(
                    scope_label(scope),
                    BlockerKind::Importing,
                    ClearsBy::AnyPeer,
                ));
            }
        }
        for record in rows(&self.conn, "SELECT scope FROM secure_evicted_logs").await? {
            let [SqlValue::Blob(scope)] = record.as_slice() else {
                return Err(Error::Corrupt("eviction guard".into()));
            };
            blockers.push(blocker(
                scope_label(&decode::<ResourceScope>(scope)?),
                BlockerKind::RestorationGuard,
                ClearsBy::AnyPeer,
            ));
        }
        Ok(blockers)
    }
    async fn finalization_blocker(&self, domain: Scope) -> Result<Option<Blocker>, Error> {
        if self.workspace.followed.as_ref().map(|(scope, _)| *scope) != Some(domain) {
            return Ok(None);
        }
        Ok(self.finalization_waits(domain).await?.map(|reason| {
            let clears_by = if reason.contains("schema switch") {
                ClearsBy::LocalWork
            } else {
                ClearsBy::AnyPeer
            };
            blocker(
                domain_label(&domain),
                BlockerKind::FinalizationWaiting { reason },
                clears_by,
            )
        }))
    }
    pub(super) async fn blockers(&self) -> Result<Vec<Blocker>, Error> {
        let mut domains: BTreeSet<Scope> = self.graphs.keys().copied().collect();
        if let Some(typed) = &self.workspace.typed {
            domains.insert(typed.domain);
        }
        let mut blockers = Vec::new();
        for domain in domains {
            blockers.extend(self.workspace_blockers(domain).await?);
            if self.graphs.contains_key(&domain) {
                blockers.extend(self.finalization_blocker(domain).await?);
            }
        }
        blockers.extend(self.dropped_tip_blockers().await?);
        blockers.extend(self.line_node_blockers()?);
        blockers.extend(self.scope_state_blockers().await?);
        blockers.extend(self.pending_blockers().await?);
        blockers.sort();
        blockers.dedup();
        Ok(blockers)
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// Every reason a scope or domain of this replica waits now, derived
    /// from stored state. The gates that refuse reads, writes and imports
    /// answer from the same predicates.
    pub async fn blockers(&self) -> Result<Vec<Blocker>, Error> {
        self.inner.lock().await.blockers().await
    }
}
