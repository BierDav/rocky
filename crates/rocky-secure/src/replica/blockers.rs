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
pub(super) fn refuse(blockers: Vec<Blocker>) -> Result<(), Error> {
    if blockers.is_empty() {
        Ok(())
    } else {
        Err(Error::Blocked(blockers))
    }
}

const STATE_TABLES: &[&str] = &[
    "secure_rebootstrap",
    "secure_compaction_lost",
    "secure_finalized",
    "secure_seal_anchors",
    "secure_seal_bounds",
    "secure_checkpoints",
    "secure_subscriptions",
    "secure_roles",
    "secure_evicted_logs",
    "secure_seal_drops",
    "secure_partitions",
    "secure_followed_tip",
];

#[derive(Default)]
pub(super) struct DomainReadiness {
    domain: Vec<Blocker>,
    scopes: BTreeMap<ResourceScope, Vec<Blocker>>,
}
impl DomainReadiness {
    fn add(&mut self, scope: ResourceScope, blocker: Blocker) {
        self.scopes.entry(scope).or_default().push(blocker);
    }
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

pub(crate) struct Readiness {
    writes: Arc<crate::sqlite::Writes>,
    tables: Vec<(String, bool)>,
    cache: std::sync::Mutex<ReadinessCache>,
}
#[derive(Default)]
struct ReadinessCache {
    generation: u64,
    domains: BTreeMap<Scope, (u64, u64, ReadinessContext, Arc<DomainReadiness>)>,
}
#[derive(PartialEq, Eq)]
struct ReadinessContext {
    authority: u64,
    manifest: Commitment,
    tip: TipId,
    followed: Option<(Scope, String)>,
    graph_sizes: Option<[usize; 5]>,
    custody: Vec<ResourceScope>,
}
impl Readiness {
    pub(super) fn new(conn: &ScopedConnection) -> Result<Self, Error> {
        let names = conn.names();
        let physical = |table: &'static str| names.physical(table).unwrap_or(table).to_owned();
        let tables = STATE_TABLES
            .iter()
            .map(|table| (physical(table), true))
            .chain([(physical("secure_scope_state"), false)])
            .collect::<Vec<_>>();
        Ok(Self {
            writes: conn.connection().watch_writes(tables.iter().cloned())?,
            tables,
            cache: Default::default(),
        })
    }
    fn cache(&self) -> std::sync::MutexGuard<'_, ReadinessCache> {
        self.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    pub(super) fn invalidate(&self) {
        let mut cache = self.cache();
        cache.generation = cache.generation.wrapping_add(1);
        cache.domains.clear();
    }
    pub(super) fn writes(&self) -> u64 {
        self.write_count()
    }
    fn write_count(&self) -> u64 {
        self.writes.selected_count(
            self.tables
                .iter()
                .map(|(table, updates)| (table.as_str(), *updates)),
        )
    }
    fn get(&self, domain: Scope, context: &ReadinessContext) -> Option<Arc<DomainReadiness>> {
        let cache = self.cache();
        let writes = self.write_count();
        cache
            .domains
            .get(&domain)
            .filter(|(generation, seen, known, _)| {
                *generation == cache.generation && *seen == writes && known == context
            })
            .map(|(.., readiness)| readiness.clone())
    }
    fn put(
        &self,
        domain: Scope,
        (generation, writes): (u64, u64),
        context: ReadinessContext,
        readiness: Arc<DomainReadiness>,
    ) {
        let mut cache = self.cache();
        if cache.generation == generation && self.write_count() == writes {
            cache
                .domains
                .insert(domain, (generation, writes, context, readiness));
        }
    }
    fn mark(&self) -> (u64, u64) {
        (self.cache().generation, self.write_count())
    }
}

impl<A: RowApplication> Inner<A> {
    fn readiness_context(&self, domain: Scope) -> Result<ReadinessContext, Error> {
        let custody = self
            .custody_scopes(now()?)?
            .into_iter()
            .filter(|scope| scope.domain == domain)
            .collect();
        let graph_sizes = self.graphs.get(&domain).map(|graph| {
            [
                graph.seals.len(),
                graph.openings.len(),
                graph.moves.len(),
                graph.checkpoints.len(),
                graph.revoked.len(),
            ]
        });
        Ok(ReadinessContext {
            authority: self.authority,
            manifest: self.manifest,
            tip: self.workspace.tip,
            followed: self.workspace.followed.clone(),
            graph_sizes,
            custody,
        })
    }
    pub(super) async fn readiness(&self, domain: Scope) -> Result<Arc<DomainReadiness>, Error> {
        let context = self.readiness_context(domain)?;
        if let Some(readiness) = self.readiness.get(domain, &context) {
            return Ok(readiness);
        }
        let mark = self.readiness.mark();
        let readiness = Arc::new(self.derive_readiness(domain).await?);
        self.readiness.put(domain, mark, context, readiness.clone());
        Ok(readiness)
    }
    async fn derive_readiness(&self, domain: Scope) -> Result<DomainReadiness, Error> {
        let mut readiness = DomainReadiness::default();
        if self.graphs.contains_key(&domain) {
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
                        FrameReplay::Peers => ClearsBy::AnyPeer,
                    },
                ),
            );
        }
        Ok(readiness)
    }
    pub(super) async fn workspace_blockers(&self, domain: Scope) -> Result<Vec<Blocker>, Error> {
        Ok(self.readiness(domain).await?.all())
    }
    pub(super) async fn domain_blockers(&self, domain: Scope) -> Result<Vec<Blocker>, Error> {
        Ok(self.readiness(domain).await?.domain.clone())
    }
    pub(super) async fn scope_blockers(&self, scope: ResourceScope) -> Result<Vec<Blocker>, Error> {
        Ok(self.readiness(scope.domain).await?.of(scope))
    }
    pub(super) async fn incomplete_blockers(
        &self,
        readiness: &DomainReadiness,
        scope: ResourceScope,
    ) -> Result<Vec<Blocker>, Error> {
        let label = scope_label(&scope);
        let mut pending = PendingCounts::default();
        let mut after = (Vec::new(), Vec::new());
        while let Some(last) = self
            .pending_heads(&mut pending, after, usize::MAX >> 1)
            .await?
        {
            after = last;
        }
        let mut blockers = readiness.of(scope);
        blockers.extend(
            pending
                .blockers()
                .into_iter()
                .filter(|blocker| blocker.scope == label),
        );
        blockers.sort();
        blockers.dedup();
        if blockers.is_empty() && self.restoring(scope).await? {
            blockers.push(blocker(
                label,
                BlockerKind::RestorationGuard,
                ClearsBy::AnyPeer,
            ));
        }
        Ok(blockers)
    }
    pub(super) async fn readable_blockers(
        &self,
        scope: ResourceScope,
    ) -> Result<Vec<Blocker>, Error> {
        let readiness = self.readiness(scope.domain).await?;
        self.readable_in(&readiness, scope).await
    }
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
    pub(super) fn line_node_blockers(&self) -> Result<Vec<Blocker>, Error> {
        let time = now()?;
        let clears_by = match &self.node_author {
            None => ClearsBy::LocalWork,
            Some(author)
                if self
                    .verified_chain(&author.credentials, &self.config.revocations, time)
                    .is_ok() =>
            {
                return Ok(Vec::new());
            }
            Some(_) => ClearsBy::Application,
        };
        let writes = self.local_grants(time)?.iter().any(|grant| {
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
                clears_by,
            )]
        } else {
            Vec::new()
        })
    }
    async fn pending_heads(
        &self,
        pending: &mut PendingCounts,
        after: (Vec<u8>, Vec<u8>),
        limit: usize,
    ) -> Result<Option<(Vec<u8>, Vec<u8>)>, Error> {
        let records = collect_rows(
            self.conn
                .query(
                    "SELECT h.scope,h.author,o.hash,h.n,q.reason FROM (SELECT scope,author,min(sequence) AS sequence,count(*) AS n FROM secure_operations WHERE status='pending' AND (scope,author)>(?,?) GROUP BY scope,author ORDER BY scope,author LIMIT ?) h JOIN secure_operations o ON o.author=h.author AND o.scope=h.scope AND o.sequence=h.sequence LEFT JOIN secure_quarantine q ON q.hash=o.hash ORDER BY h.scope,h.author",
                    bindings(vec![
                        SqlValue::Blob(after.0),
                        SqlValue::Blob(after.1),
                        SqlValue::Integer(limit as i64),
                    ]),
                )
                .await?,
        )
        .await?;
        let mut last = None;
        let full = records.len() == limit;
        for record in records {
            let [
                SqlValue::Blob(scope),
                SqlValue::Blob(author),
                SqlValue::Blob(hash),
                SqlValue::Integer(count),
                failure,
            ] = record.as_slice()
            else {
                return Err(Error::Corrupt("pending operation".into()));
            };
            last = Some((scope.clone(), author.clone()));
            let scope: ResourceScope = decode(scope)?;
            if let SqlValue::Text(reason) = failure {
                *pending
                    .quarantined
                    .entry((scope_label(&scope), reason.clone()))
                    .or_default() += *count as u64;
                continue;
            }
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
            if let Some((author, stamp)) = self.ahead.get(&hash) {
                let seconds = stamp.saturating_sub(micros()?).div_ceil(1_000_000);
                let entry = pending
                    .ahead
                    .entry((scope_label(&scope), author.to_hex()))
                    .or_default();
                entry.0 += *count as u64;
                entry.1 = entry.1.max(seconds);
                continue;
            }
            *pending
                .counts
                .entry((scope_label(&scope), reason))
                .or_default() += *count as u64;
        }
        Ok(last.filter(|_| full))
    }
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
            let scope = decode::<ResourceScope>(scope)?;
            if !self.holds(scope).await? {
                continue;
            }
            blockers.push(blocker(
                scope_label(&scope),
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
}

#[derive(Default)]
struct PendingCounts {
    counts: BTreeMap<(String, WaitReason), u64>,
    quarantined: BTreeMap<(String, String), u64>,
    ahead: BTreeMap<(String, String), (u64, u64)>,
}
impl PendingCounts {
    fn blockers(self) -> Vec<Blocker> {
        let undecryptable = |reason: &str| reason == super::quarantine::UNDECRYPTABLE;
        self.counts
            .into_iter()
            .map(|((scope, reason), count)| {
                blocker(
                    scope,
                    BlockerKind::PendingFrames { reason, count },
                    reason.clears_by(),
                )
            })
            .chain(
                self.quarantined
                    .into_iter()
                    .map(|((scope, reason), count)| {
                        let clears_by = if undecryptable(&reason) {
                            ClearsBy::AnyPeer
                        } else {
                            ClearsBy::LocalWork
                        };
                        blocker(scope, BlockerKind::Quarantined { reason, count }, clears_by)
                    }),
            )
            .chain(
                self.ahead
                    .into_iter()
                    .map(|((scope, node), (count, seconds))| {
                        blocker(
                            scope,
                            BlockerKind::ClockAhead {
                                node,
                                count,
                                seconds,
                            },
                            ClearsBy::Clock,
                        )
                    }),
            )
            .collect()
    }
}

const PENDING_HEADS: usize = if cfg!(test) { 1 } else { 64 };
const BLOCKER_PASSES: usize = 4;
const LISTING_TABLES: &[&str] = &[
    "secure_quarantine",
    "secure_scope_state",
    "secure_position_ranges",
    "secure_logs",
];

struct BlockerSource {
    writes: Arc<crate::sqlite::Writes>,
    tables: Vec<String>,
    count: u64,
    authority: u64,
    tip: TipId,
}
impl BlockerSource {
    fn start<A: RowApplication>(inner: &Inner<A>) -> Result<Self, Error> {
        let names = inner.conn.names();
        let tables: Vec<String> = STATE_TABLES
            .iter()
            .chain(LISTING_TABLES)
            .map(|table| names.physical(table).unwrap_or(table).to_owned())
            .collect();
        let writes = inner
            .conn
            .connection()
            .watch_writes(tables.iter().map(|table| (table.clone(), true)))?;
        Ok(Self {
            count: writes.selected_count(tables.iter().map(|table| (table.as_str(), true))),
            writes,
            tables,
            authority: inner.authority,
            tip: inner.workspace.tip,
        })
    }
    fn check<A: RowApplication>(&self, inner: &Inner<A>) -> Result<(), Error> {
        let count = self
            .writes
            .selected_count(self.tables.iter().map(|table| (table.as_str(), true)));
        if count != self.count
            || self.authority != inner.authority
            || self.tip != inner.workspace.tip
        {
            return Err(Error::RetryExhausted);
        }
        Ok(())
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    pub async fn blockers(&self) -> Result<Vec<Blocker>, Error> {
        for _ in 0..BLOCKER_PASSES {
            match self.blocker_pass().await {
                Err(Error::RetryExhausted) => continue,
                result => return result,
            }
        }
        let inner = self.inner.lock(Class::Background).await;
        #[cfg(any(test, feature = "qualification"))]
        inner
            .flusher
            .measurements
            .work(super::lock::WorkUnit::ScanFallback, 1);
        let mut blockers = Vec::new();
        for domain in blocker_domains(&inner) {
            blockers.extend(inner.domain_listing(domain).await?);
        }
        blockers.extend(inner.state_listing().await?);
        let mut pending = PendingCounts::default();
        let mut after = (Vec::new(), Vec::new());
        while let Some(last) = inner
            .pending_heads(&mut pending, after, usize::MAX >> 1)
            .await?
        {
            after = last;
        }
        Ok(listed(blockers, pending))
    }
    async fn blocker_pass(&self) -> Result<Vec<Blocker>, Error> {
        let (source, domains) = {
            let inner = self.inner.lock(Class::Background).await;
            (BlockerSource::start(&inner)?, blocker_domains(&inner))
        };
        let step = async || {
            rocky_platform::task::yield_now().await;
            let inner = self.inner.lock(Class::Background).await;
            source.check(&inner)?;
            Ok::<_, Error>(inner)
        };
        let mut blockers = Vec::new();
        for domain in domains {
            blockers.extend(step().await?.domain_listing(domain).await?);
        }
        blockers.extend(step().await?.state_listing().await?);
        let mut pending = PendingCounts::default();
        let mut after = (Vec::new(), Vec::new());
        loop {
            let inner = step().await?;
            match inner
                .pending_heads(&mut pending, after, PENDING_HEADS)
                .await?
            {
                Some(last) => after = last,
                None => return Ok(listed(blockers, pending)),
            }
        }
    }
}

fn blocker_domains<A: RowApplication>(inner: &Inner<A>) -> BTreeSet<Scope> {
    let mut domains: BTreeSet<Scope> = inner.graphs.keys().copied().collect();
    if let Some(typed) = &inner.workspace.typed {
        domains.insert(typed.domain);
    }
    domains
}
fn listed(mut blockers: Vec<Blocker>, pending: PendingCounts) -> Vec<Blocker> {
    blockers.extend(pending.blockers());
    blockers.sort();
    blockers.dedup();
    blockers
}
impl<A: RowApplication> Inner<A> {
    async fn domain_listing(&self, domain: Scope) -> Result<Vec<Blocker>, Error> {
        let mut blockers = self.workspace_blockers(domain).await?;
        if self.graphs.contains_key(&domain) {
            blockers.extend(self.finalization_blocker(domain).await?);
        }
        Ok(blockers)
    }
    async fn state_listing(&self) -> Result<Vec<Blocker>, Error> {
        let mut blockers = self.line_node_blockers()?;
        blockers.extend(self.scope_state_blockers().await?);
        blockers.extend(self.lane_blockers().await?);
        Ok(blockers)
    }
}
