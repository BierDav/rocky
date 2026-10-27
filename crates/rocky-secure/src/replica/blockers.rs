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

/// The stored state blockers derive from, apart from the per-frame tables
/// a transaction boundary covers: a change to any of them, even inside one
/// transaction, makes cached readiness stale. Readiness reads only which
/// scopes have state, so `secure_scope_state`'s per-frame updates do not
/// count.
const STATE_TABLES: &[&str] = &[
    "secure_rebootstrap",
    "secure_compaction_lost",
    "secure_finalized",
    "secure_seal_anchors",
    "secure_seal_bounds",
    "secure_checkpoints",
    "secure_existence_base",
    "secure_subscriptions",
    "secure_roles",
    "secure_evicted_logs",
    "secure_seal_drops",
    "secure_partitions",
    "secure_followed_tip",
];

/// One domain's blockers: those every scope shares, kept domain-wide by the
/// phase 7 inventory (`docs/features_11/07-scope-local-gates-evidence.md`),
/// and each scope's own.
#[derive(Default)]
pub(super) struct DomainReadiness {
    domain: Vec<Blocker>,
    scopes: BTreeMap<ResourceScope, Vec<Blocker>>,
}
impl DomainReadiness {
    fn add(&mut self, scope: ResourceScope, blocker: Blocker) {
        self.scopes.entry(scope).or_default().push(blocker);
    }
    /// The domain's blockers and `scope`'s own.
    fn of(&self, scope: ResourceScope) -> Vec<Blocker> {
        let mut blockers = self.domain.clone();
        blockers.extend(self.scopes.get(&scope).into_iter().flatten().cloned());
        blockers
    }
    fn all(&self) -> Vec<Blocker> {
        let mut blockers = self.domain.clone();
        blockers.extend(self.scopes.values().flatten().cloned());
        blockers
    }
}

/// Derived readiness per domain, valid for one generation, one count of
/// state-table writes and one context (tip, authority, graph, custody).
pub(crate) struct Readiness {
    writes: Arc<crate::sqlite::Writes>,
    cache: std::sync::Mutex<ReadinessCache>,
}
#[derive(Default)]
struct ReadinessCache {
    generation: u64,
    domains: BTreeMap<Scope, (u64, u64, Commitment, Arc<DomainReadiness>)>,
    #[cfg(test)]
    derivations: u64,
}
impl Readiness {
    pub(super) fn new(conn: &ScopedConnection) -> Result<Self, Error> {
        let names = conn.names();
        let physical = |table: &'static str| names.physical(table).unwrap_or(table).to_owned();
        let tables = STATE_TABLES
            .iter()
            .map(|table| (physical(table), true))
            .chain([(physical("secure_scope_state"), false)]);
        Ok(Self {
            writes: conn.connection().watch_writes(tables)?,
            cache: Default::default(),
        })
    }
    fn cache(&self) -> std::sync::MutexGuard<'_, ReadinessCache> {
        self.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    /// Before a write attempt, after a commit and after a rollback.
    pub(super) fn invalidate(&self) {
        let mut cache = self.cache();
        cache.generation = cache.generation.wrapping_add(1);
        cache.domains.clear();
    }
    fn get(&self, domain: Scope, context: Commitment) -> Option<Arc<DomainReadiness>> {
        let cache = self.cache();
        let writes = self.writes.count();
        cache
            .domains
            .get(&domain)
            .filter(|(generation, seen, known, _)| {
                *generation == cache.generation && *seen == writes && *known == context
            })
            .map(|(.., readiness)| readiness.clone())
    }
    /// Keeps `readiness`, derived since `writes` state-table writes, unless
    /// a write or an invalidation happened meanwhile.
    fn put(
        &self,
        domain: Scope,
        (generation, writes): (u64, u64),
        context: Commitment,
        readiness: Arc<DomainReadiness>,
    ) {
        let mut cache = self.cache();
        #[cfg(test)]
        {
            cache.derivations += 1;
        }
        if cache.generation == generation && self.writes.count() == writes {
            cache
                .domains
                .insert(domain, (generation, writes, context, readiness));
        }
    }
    fn mark(&self) -> (u64, u64) {
        (self.cache().generation, self.writes.count())
    }
    #[cfg(test)]
    pub(crate) fn derivations(&self) -> u64 {
        self.cache().derivations
    }
}

impl<A: RowApplication> Inner<A> {
    /// What the cached readiness of `domain` depends on besides stored state.
    fn readiness_context(&self, domain: Scope) -> Result<Commitment, Error> {
        let custody: Vec<_> = self
            .custody_scopes(now()?)?
            .into_iter()
            .filter(|scope| scope.domain == domain)
            .collect();
        let counts = self.graphs.get(&domain).map(|graph| {
            [
                graph.seals.len(),
                graph.openings.len(),
                graph.moves.len(),
                graph.finalities.len(),
                graph.checkpoints.len(),
                graph.revoked.len(),
            ]
        });
        Ok(rocky_core::commitment(
            "rocky.readiness-context.v1",
            &encode(&(
                self.authority,
                self.manifest,
                self.workspace.tip,
                &self.workspace.followed,
                counts,
                custody,
            ))?,
        ))
    }
    /// The blockers of `domain`, derived once per change of what they read.
    pub(super) async fn readiness(&self, domain: Scope) -> Result<Arc<DomainReadiness>, Error> {
        let context = self.readiness_context(domain)?;
        if let Some(readiness) = self.readiness.get(domain, context) {
            return Ok(readiness);
        }
        let mark = self.readiness.mark();
        let readiness = Arc::new(self.derive_readiness(domain).await?);
        self.readiness.put(domain, mark, context, readiness.clone());
        Ok(readiness)
    }
    /// An import outside a seal and a base that must move to a seal of the
    /// followed path hold back their own scope; a lost compaction holds
    /// back the whole domain.
    async fn derive_readiness(&self, domain: Scope) -> Result<DomainReadiness, Error> {
        let mut readiness = DomainReadiness::default();
        if self.graphs.contains_key(&domain) {
            // An evicted scope reads as unavailable; its own log returning
            // holds back only authoring.
            for (scope, id) in self.domain_scopes(domain).await? {
                if let Some(staging) = self.staging(id).await?
                    && staging.signed.statement.seal.is_none()
                {
                    readiness.add(
                        scope,
                        blocker(
                            scope_label(&scope),
                            BlockerKind::StagedImport {
                                sealed: false,
                                pages_complete: staging.complete,
                            },
                            ClearsBy::AnyPeer,
                        ),
                    );
                }
            }
        }
        let needs = self.seal_rebootstraps(domain).await?;
        if self.compaction_lost(domain).await?.is_some() {
            readiness.domain.push(blocker(
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
            readiness.add(
                need.scope,
                blocker(
                    scope_label(&need.scope),
                    BlockerKind::SealRebootstrap {
                        replay: need.replay,
                        listed: need.listed.len() as u64,
                    },
                    match need.replay {
                        FrameReplay::Local => ClearsBy::LocalWork,
                        // Only a listed checkpoint replaces released existence
                        // records; a trusted peer holding one lists it first.
                        FrameReplay::Unavailable if need.listed.is_empty() => ClearsBy::NoPeer,
                        FrameReplay::Peers | FrameReplay::Unavailable => ClearsBy::AnyPeer,
                    },
                ),
            );
        }
        Ok(readiness)
    }
    /// Every readiness blocker of `domain`, for diagnostics.
    pub(super) async fn workspace_blockers(&self, domain: Scope) -> Result<Vec<Blocker>, Error> {
        Ok(self.readiness(domain).await?.all())
    }
    /// What holds back every scope of `domain`.
    pub(super) async fn domain_blockers(&self, domain: Scope) -> Result<Vec<Blocker>, Error> {
        Ok(self.readiness(domain).await?.domain.clone())
    }
    /// What holds back authoring in `scope`: its domain's blockers and its own.
    pub(super) async fn scope_blockers(&self, scope: ResourceScope) -> Result<Vec<Blocker>, Error> {
        Ok(self.readiness(scope.domain).await?.of(scope))
    }
    /// What holds back reading `scope`: as for authoring, except that a
    /// scope that waits for a rebootstrap reads the verified pages of a
    /// direct import.
    pub(super) async fn readable_blockers(
        &self,
        scope: ResourceScope,
    ) -> Result<Vec<Blocker>, Error> {
        let readiness = self.readiness(scope.domain).await?;
        self.readable_in(&readiness, scope).await
    }
    /// `readable_blockers` from `scope`'s domain readiness.
    pub(super) async fn readable_in(
        &self,
        readiness: &DomainReadiness,
        scope: ResourceScope,
    ) -> Result<Vec<Blocker>, Error> {
        let mut blockers = readiness.of(scope);
        if blockers
            .iter()
            .any(|blocker| matches!(blocker.kind, BlockerKind::SealRebootstrap { .. }))
            && self.authenticated_direct_import(scope).await?
        {
            blockers.retain(|blocker| !matches!(blocker.kind, BlockerKind::SealRebootstrap { .. }));
        }
        Ok(blockers)
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
    /// Pending frames by scope and the reason their log heads wait; a
    /// quarantined head names its failure instead.
    async fn pending_blockers(&self) -> Result<Vec<Blocker>, Error> {
        let mut counts: BTreeMap<(String, WaitReason), u64> = BTreeMap::new();
        let mut quarantined: BTreeMap<(String, String), u64> = BTreeMap::new();
        for record in rows(
            &self.conn,
            "SELECT o.scope,o.hash,h.n,q.reason FROM secure_operations o JOIN (SELECT scope,author,min(sequence) AS sequence,count(*) AS n FROM secure_operations WHERE status='pending' GROUP BY scope,author) h ON o.author=h.author AND o.scope=h.scope AND o.sequence=h.sequence LEFT JOIN secure_quarantine q ON q.hash=o.hash WHERE o.status='pending'",
        )
        .await?
        {
            let [
                SqlValue::Blob(scope),
                SqlValue::Blob(hash),
                SqlValue::Integer(count),
                failure,
            ] = record.as_slice()
            else {
                return Err(Error::Corrupt("pending operation".into()));
            };
            if let SqlValue::Text(reason) = failure {
                let scope: ResourceScope = decode(scope)?;
                *quarantined
                    .entry((scope_label(&scope), reason.clone()))
                    .or_default() += *count as u64;
                continue;
            }
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
        let undecryptable = |reason: &str| reason == super::quarantine::UNDECRYPTABLE;
        Ok(counts
            .into_iter()
            .map(|((scope, reason), count)| {
                blocker(
                    scope,
                    BlockerKind::PendingFrames { reason, count },
                    reason.clears_by(),
                )
            })
            .chain(quarantined.into_iter().map(|((scope, reason), count)| {
                // Another copy of the key or the frame comes from a peer; any
                // other failure is retried here as what it depends on changes.
                let clears_by = if undecryptable(&reason) {
                    ClearsBy::AnyPeer
                } else {
                    ClearsBy::LocalWork
                };
                blocker(scope, BlockerKind::Quarantined { reason, count }, clears_by)
            }))
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
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .blockers()
            .await
    }
}
