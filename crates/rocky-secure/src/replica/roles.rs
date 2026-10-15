//! Replica roles, scope subscriptions, per-log frontiers and range transfer.
//! Rules: `docs/runtime/replica-roles.md`.
use super::checkpoint::{MAX_CHECKPOINT_LOGS, key_of};
use super::*;
use rocky_credentials::PeerId;

pub(crate) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_roles(id INTEGER PRIMARY KEY CHECK(id=1),config BLOB NOT NULL)",
    "CREATE TABLE IF NOT EXISTS secure_subscriptions(scope BLOB PRIMARY KEY,state TEXT NOT NULL)",
    "CREATE TABLE IF NOT EXISTS secure_evicted_logs(scope BLOB PRIMARY KEY,sequence INTEGER NOT NULL,hash BLOB NOT NULL)",
    "CREATE TABLE IF NOT EXISTS secure_peer_frontiers(peer BLOB NOT NULL,scope BLOB NOT NULL,author BLOB NOT NULL,held INTEGER NOT NULL,applied INTEGER NOT NULL,PRIMARY KEY(peer,scope,author)) WITHOUT ROWID",
    "CREATE INDEX IF NOT EXISTS secure_operations_scope ON secure_operations(scope,author,sequence)",
];

/// Frames in one range page.
pub(crate) const RANGE_FRAMES: usize = 32;

/// What this replica holds. Local configuration, never taken from a peer.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum ReplicaRole {
    /// Holds every scope it is authorized for and receives; never evicts.
    #[default]
    Full,
    /// Holds only subscribed scopes and may evict them.
    Partial,
}
/// The local role and the peers this replica relies on as full replica nodes.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoleConfig {
    pub role: ReplicaRole,
    /// Peers trusted as full replica nodes, pinned by key.
    pub full_nodes: BTreeSet<PeerId>,
    /// Full nodes that must have applied an operation before it is pruned or
    /// evicted. Zero keeps pruning on the retention policy alone; eviction
    /// always needs at least one.
    pub custody: usize,
}
/// Whether a scope is held, and where an eviction stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Holding {
    /// Subscribed, or any received scope of a full replica.
    Held,
    /// Unsubscribed: rows stay but no longer follow the peers.
    Left,
    /// An eviction is in progress; reads are unavailable.
    Evicting,
    /// Rows and history are gone; resubscribe to hold the scope again.
    Evicted,
}
impl Holding {
    fn label(self) -> &'static str {
        match self {
            Self::Held => "held",
            Self::Left => "left",
            Self::Evicting => "evicting",
            Self::Evicted => "evicted",
        }
    }
    fn parse(label: &str) -> Result<Self, Error> {
        Ok(match label {
            "held" => Self::Held,
            "left" => Self::Left,
            "evicting" => Self::Evicting,
            "evicted" => Self::Evicted,
            _ => return Err(Error::Corrupt("subscription state".into())),
        })
    }
}
/// One author's log in a scope: the next sequence not held, received or
/// covered by a checkpoint, and the next sequence not applied.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FrontierLog {
    pub author: PublicKey,
    pub held: u64,
    pub applied: u64,
}
/// The contiguous prefix of every log of a scope a replica holds.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScopeFrontier {
    pub scope: ResourceScope,
    /// Ordered by author, at most `MAX_CHECKPOINT_LOGS`.
    pub logs: Vec<FrontierLog>,
    /// Cut records of the scope's coordinated log the holder has.
    pub cuts: u64,
}
impl ScopeFrontier {
    pub fn log(&self, author: PublicKey) -> Option<&FrontierLog> {
        self.logs.iter().find(|log| log.author == author)
    }
    fn held(&self, author: PublicKey) -> u64 {
        self.log(author).map_or(0, |log| log.held)
    }
    /// Applied operations `other` has and this frontier lacks.
    pub fn lag_behind(&self, other: &Self) -> u64 {
        other
            .logs
            .iter()
            .map(|log| {
                log.applied
                    .saturating_sub(self.log(log.author).map_or(0, |own| own.applied))
            })
            .sum()
    }
    pub(crate) fn validate(&self) -> Result<(), Error> {
        if self.logs.len() > MAX_CHECKPOINT_LOGS
            || self
                .logs
                .windows(2)
                .any(|pair| pair[0].author >= pair[1].author)
            || self.logs.iter().any(|log| log.applied > log.held)
        {
            return Err(denied("invalid scope frontier"));
        }
        Ok(())
    }
}
pub(crate) const MAX_FRONTIER_SCOPES: usize = 64;

#[derive(Serialize, Deserialize)]
pub(crate) struct FrontierState {
    pub(crate) frontier: ScopeFrontier,
    pub(crate) signed: Option<Box<rocky_credentials::SignedHorizon>>,
    pub(crate) behind: bool,
    /// The replying replica imports the scope from a checkpoint and takes
    /// no history for it meanwhile.
    pub(crate) importing: bool,
}
/// One member's answer in a frontier batch, in request order.
#[derive(Serialize, Deserialize)]
pub(crate) enum FrontierReply {
    Held(FrontierState),
    /// The asking peer may not hold the whole scope here, including a
    /// partition whose tuple this replica has not learned. Only this member
    /// is skipped; the session goes on.
    Refused(ResourceScope),
}
impl FrontierReply {
    pub(crate) fn scope(&self) -> ResourceScope {
        match self {
            Self::Held(state) => state.frontier.scope,
            Self::Refused(scope) => *scope,
        }
    }
}

/// Whether `grants` let their holder keep the whole scope, by read or custody.
/// A partition's selector grants resolve only against a learned tuple.
pub(crate) fn holds_whole(grants: &[rocky_core::ResourceGrant], scope: ResourceScope) -> bool {
    grants.iter().any(|grant| {
        grant.scope == scope
            && grant.rows == RowSelection::All
            && (grant.rights.contains(&Right::Custody) || grant.rights.contains(&Right::Read))
    })
}

pub(crate) fn validate_frontiers(frontiers: &[ScopeFrontier]) -> Result<(), Error> {
    if frontiers.is_empty()
        || frontiers.len() > MAX_FRONTIER_SCOPES
        || frontiers
            .iter()
            .map(|frontier| frontier.scope)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != frontiers.len()
    {
        return Err(denied("invalid frontier batch"));
    }
    Ok(())
}

/// Frames a peer lacks, in author and sequence order.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RangePage {
    pub scope: ResourceScope,
    pub frames: Vec<EncryptedFrame>,
    /// Continue after this log position; `None` ends the pass.
    pub next: Option<(PublicKey, u64)>,
    /// The peer is behind this replica's horizon and must rebootstrap; no
    /// frames are sent.
    pub behind: bool,
    /// The sender's own frontier, for lag reporting.
    pub frontier: ScopeFrontier,
    /// Void records consuming positions of the logs the page continues.
    pub voids: Vec<SignedVoid>,
    /// Cut records of the scope's log the requester lacks; received first.
    pub cuts: Vec<crate::replica::CutEvidence>,
}
/// Lag between this replica and one peer in one scope, by applied operations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerLag {
    pub peer: PeerId,
    /// Operations this replica applied that the peer last reported lacking.
    pub behind: u64,
    /// Operations the peer last reported applied that this replica lacks.
    pub ahead: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeStatus {
    pub scope: ResourceScope,
    pub holding: Holding,
    pub rows: u64,
    /// Retained operations at or before the verified checkpoint.
    pub covered: u64,
    /// Retained applied operations after it.
    pub uncovered: u64,
    /// Logs with pruned history.
    pub pruned_logs: usize,
    /// Operations this node authored in the scope.
    pub local_operations: u64,
    /// Per configured full node: local operations it has applied.
    pub custody: Vec<(PeerId, u64)>,
    pub peers: Vec<PeerLag>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplicaStatus {
    pub role: RoleConfig,
    pub scopes: Vec<ScopeStatus>,
}

impl<A: RowApplication> Inner<A> {
    pub(crate) async fn role(&self) -> Result<RoleConfig, Error> {
        let stored = rows(&self.conn, "SELECT config FROM secure_roles WHERE id=1").await?;
        match stored.first().map(Vec::as_slice) {
            Some([SqlValue::Blob(bytes)]) => decode(bytes),
            None => Ok(RoleConfig::default()),
            _ => Err(Error::Corrupt("role configuration".into())),
        }
    }
    pub(crate) async fn holding(&self, scope: ResourceScope) -> Result<Option<Holding>, Error> {
        let mut result = self
            .conn
            .query(
                "SELECT state FROM secure_subscriptions WHERE scope=?",
                [encode(&scope)?],
            )
            .await?;
        match result.next().await? {
            Some(row) => Ok(Some(Holding::parse(&row.get::<String>(0)?)?)),
            None => Ok(None),
        }
    }
    /// Whether rows of the scope follow the peers here: subscribed, or any
    /// scope of a full replica that is not being evicted.
    pub(crate) async fn holds(&self, scope: ResourceScope) -> Result<bool, Error> {
        Ok(match self.holding(scope).await? {
            Some(Holding::Held) => true,
            Some(Holding::Evicting | Holding::Evicted) => false,
            Some(Holding::Left) | None => self.role().await?.role == ReplicaRole::Full,
        })
    }
    async fn set_holding(&self, scope: ResourceScope, holding: Holding) -> Result<(), Error> {
        self.conn
            .execute(
                "INSERT INTO secure_subscriptions VALUES(?,?) ON CONFLICT(scope) DO UPDATE SET state=excluded.state",
                bindings(vec![
                    SqlValue::Blob(encode(&scope)?),
                    SqlValue::Text(holding.label().into()),
                ]),
            )
            .await?;
        Ok(())
    }
    /// The own log tip an eviction dropped. Authoring waits until the log is back.
    async fn eviction_guard(&self, scope: ResourceScope) -> Result<Option<i64>, Error> {
        let mut result = self
            .conn
            .query(
                "SELECT sequence FROM secure_evicted_logs WHERE scope=?",
                [encode(&scope)?],
            )
            .await?;
        Ok(match result.next().await? {
            Some(row) => Some(row.get::<i64>(0)?),
            None => None,
        })
    }
    /// Refuses frames of a scope this replica does not hold. A frame of the
    /// local signing identity is accepted only as a copy of what an eviction
    /// dropped.
    pub(crate) async fn admit_scope(
        &self,
        header: &p2panda_core::Header<Extensions>,
    ) -> Result<(), Error> {
        let scope = header.extensions.scope;
        if !self.holds(scope).await? {
            return Err(denied("scope is not held by this replica"));
        }
        if header.public_key == self.config.node.public_key()
            && self
                .eviction_guard(scope)
                .await?
                .is_none_or(|sequence| header.seq_num as i64 > sequence)
        {
            return Err(denied("unknown operation from local signing identity"));
        }
        Ok(())
    }
    /// Authoring needs a held scope whose own log is complete again.
    pub(crate) async fn admit_author(
        &self,
        scope: ResourceScope,
        tip: Option<i64>,
    ) -> Result<(), Error> {
        if !self.holds(scope).await? {
            return Err(denied("author needs a held scope"));
        }
        if let Some(guard) = self.eviction_guard(scope).await? {
            if tip.is_none_or(|tip| tip < guard) {
                return Err(denied(
                    "catch up the resubscribed scope before authoring in it",
                ));
            }
            self.conn
                .execute(
                    "DELETE FROM secure_evicted_logs WHERE scope=?",
                    [encode(&scope)?],
                )
                .await?;
        }
        Ok(())
    }
    /// Records the own log tip an eviction drops, -1 without one. The scope
    /// reads as unavailable until a catch-up restores it.
    pub(super) async fn save_eviction_guard(&self, scope: ResourceScope) -> Result<(), Error> {
        let log = match self.find_scope_id(scope).await? {
            Some(id) => self.log(id, self.config.node.public_key()).await?,
            None => None,
        };
        self.conn
            .execute(
                "INSERT INTO secure_evicted_logs VALUES(?,?,?) ON CONFLICT(scope) DO UPDATE SET sequence=MAX(sequence,excluded.sequence),hash=excluded.hash",
                bindings(vec![
                    SqlValue::Blob(encode(&scope)?),
                    SqlValue::Integer(log.as_ref().map_or(-1, |log| log.sequence)),
                    SqlValue::Blob(log.map_or_else(Vec::new, |log| log.hash.as_bytes().to_vec())),
                ]),
            )
            .await?;
        Ok(())
    }
    /// Whether a resubscribed scope still waits for the catch-up that
    /// restores what its eviction dropped.
    pub(crate) async fn restoring(&self, scope: ResourceScope) -> Result<bool, Error> {
        Ok(self.eviction_guard(scope).await?.is_some())
    }
    pub(super) async fn mark_evicting(
        &self,
        scope: ResourceScope,
        done: bool,
    ) -> Result<(), Error> {
        self.set_holding(
            scope,
            if done {
                Holding::Evicted
            } else {
                Holding::Evicting
            },
        )
        .await
    }
    /// The contiguous held and applied prefix of every log of a scope.
    pub(crate) async fn frontier(&self, scope: ResourceScope) -> Result<ScopeFrontier, Error> {
        if self.graphs.contains_key(&scope.domain) {
            return self.tip_frontier(scope).await;
        }
        let mut logs = BTreeMap::<PublicKey, (u64, u64)>::new();
        if let Some(id) = self.find_scope_id(scope).await? {
            for (log, _) in self.logs(id, None, false, MAX_CHECKPOINT_LOGS + 1).await? {
                logs.insert(log.author, (log.sequence + 1, log.sequence + 1));
            }
        }
        let blob = encode(&scope)?;
        for record in collect_rows(
            self.conn
                .query(
                    "SELECT author FROM secure_operations WHERE scope=?1 UNION SELECT author FROM secure_voids WHERE scope=?1",
                    [blob.clone()],
                )
                .await?,
        )
        .await?
        {
            let [SqlValue::Blob(author)] = record.as_slice() else {
                return Err(Error::Corrupt("operation author".into()));
            };
            let author = key_of(author)?;
            let entry = logs.entry(author).or_insert((0, 0));
            // Received operations extend the prefix while they are contiguous.
            loop {
                let next = collect_rows(
                    self.conn
                        .query(
                            // A voided position is consumed: it continues the prefix.
                            "SELECT sequence FROM secure_operations WHERE scope=?1 AND author=?2 AND sequence>=?3 UNION SELECT sequence FROM secure_voids WHERE scope=?1 AND author=?2 AND sequence>=?3 ORDER BY sequence LIMIT 256",
                            bindings(vec![
                                SqlValue::Blob(blob.clone()),
                                SqlValue::Blob(author.as_bytes().to_vec()),
                                SqlValue::Integer(entry.0 as i64),
                            ]),
                        )
                        .await?,
                )
                .await?;
                let before = entry.0;
                for record in &next {
                    match record.as_slice() {
                        [SqlValue::Integer(sequence)] if *sequence as u64 == entry.0 => {
                            entry.0 += 1
                        }
                        _ => break,
                    }
                }
                if next.len() < 256 || entry.0 - before < next.len() as u64 {
                    break;
                }
            }
        }
        if logs.len() > MAX_CHECKPOINT_LOGS {
            return Err(Error::Quota {
                resource: "logs per scope",
                limit: MAX_CHECKPOINT_LOGS,
            });
        }
        Ok(ScopeFrontier {
            scope,
            cuts: self.held_cuts(self.tip_log(scope.domain)).len() as u64,
            logs: logs
                .into_iter()
                .map(|(author, (held, applied))| FrontierLog {
                    author,
                    held,
                    applied,
                })
                .collect(),
        })
    }
    /// The grants a peer's credentials give it now.
    fn peer_grants(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
    ) -> Result<Vec<rocky_core::ResourceGrant>, Error> {
        self.verify_chain(credentials, peer, now()?)
    }
    /// Records what a peer reported holding and applying. Application
    /// counts only from a peer that may read the whole scope.
    pub(crate) async fn record_frontier(
        &mut self,
        peer: PublicKey,
        credentials: &CredentialChain,
        frontier: &ScopeFrontier,
    ) -> Result<(), Error> {
        self.record_frontiers(peer, credentials, std::slice::from_ref(frontier))
            .await
    }
    pub(crate) async fn record_frontiers(
        &mut self,
        peer: PublicKey,
        credentials: &CredentialChain,
        frontiers: &[ScopeFrontier],
    ) -> Result<(), Error> {
        for frontier in frontiers {
            frontier.validate()?;
        }
        if peer == self.config.node.public_key() {
            return Err(denied("a replica cannot acknowledge itself"));
        }
        if self.peer_tip(peer) != Some(self.tip) {
            return Ok(());
        }
        let scopes: Vec<_> = frontiers.iter().map(|frontier| frontier.scope).collect();
        let grants = self.verify_chain_scopes(credentials, peer, now()?, &scopes)?;
        self.store_frontiers(peer, &grants, frontiers).await
    }
    /// Records frontiers under already verified `grants`; each member needs
    /// whole-scope read or custody.
    async fn store_frontiers(
        &mut self,
        peer: PublicKey,
        grants: &[rocky_core::ResourceGrant],
        frontiers: &[ScopeFrontier],
    ) -> Result<(), Error> {
        let mut reports = Vec::new();
        for frontier in frontiers {
            let whole = |right: Right| {
                grants.iter().any(|grant| {
                    grant.scope == frontier.scope
                        && grant.rows == RowSelection::All
                        && grant.rights.contains(&right)
                })
            };
            let reads = whole(Right::Read);
            if !reads && !whole(Right::Custody) {
                return Err(denied("acknowledgment right denied"));
            }
            reports.push((encode(&frontier.scope)?, frontier.logs.clone(), reads));
        }
        self.touch_peer(peer);
        let first = reports.iter().any(|(_, logs, _)| !logs.is_empty())
            && self.unrecorded_peer(peer).await?;
        transaction!(self, |inner| {
            for (scope, logs, reads) in &reports {
                for log in logs {
                    inner.conn.execute(
                        "INSERT INTO secure_peer_frontiers VALUES(?,?,?,?,?) ON CONFLICT(peer,scope,author) DO UPDATE SET held=MAX(held,excluded.held),applied=MAX(applied,excluded.applied)",
                        bindings(vec![
                            SqlValue::Blob(PeerId::of(peer).as_bytes().to_vec()),
                            SqlValue::Blob(scope.clone()),
                            SqlValue::Blob(log.author.as_bytes().to_vec()),
                            SqlValue::Integer(log.held.min(i64::MAX as u64) as i64),
                            SqlValue::Integer(if *reads {
                                log.applied.min(i64::MAX as u64) as i64
                            } else {
                                0
                            }),
                        ]),
                    ).await?;
                }
            }
            if first {
                inner.bound_peers().await?;
            }
            Ok(())
        })?;
        // A lost report only delays pruning, which flushes before publishing.
        Ok(())
    }
    /// What `peer` last reported for one log: (held, applied) next sequences.
    pub(crate) async fn peer_mark(
        &self,
        peer: PeerId,
        scope: &[u8],
        author: PublicKey,
    ) -> Result<(u64, u64), Error> {
        let mut result = self
            .conn
            .query(
                "SELECT held,applied FROM secure_peer_frontiers WHERE peer=? AND scope=? AND author=?",
                bindings(vec![
                    SqlValue::Blob(peer.as_bytes().to_vec()),
                    SqlValue::Blob(scope.to_vec()),
                    SqlValue::Blob(author.as_bytes().to_vec()),
                ]),
            )
            .await?;
        Ok(match result.next().await? {
            Some(row) => (row.get::<i64>(0)? as u64, row.get::<i64>(1)? as u64),
            None => (0, 0),
        })
    }
    /// Peers whose frontier covers one operation: held, and applied.
    pub(crate) async fn covering_peers(
        &self,
        scope: &[u8],
        author: PublicKey,
        sequence: u64,
    ) -> Result<(BTreeSet<PeerId>, BTreeSet<PeerId>), Error> {
        let mut held = BTreeSet::new();
        let mut applied = BTreeSet::new();
        for record in collect_rows(
            self.conn
                .query(
                    "SELECT peer,applied>? FROM secure_peer_frontiers WHERE scope=? AND author=? AND held>?",
                    bindings(vec![
                        SqlValue::Integer(sequence as i64),
                        SqlValue::Blob(scope.to_vec()),
                        SqlValue::Blob(author.as_bytes().to_vec()),
                        SqlValue::Integer(sequence as i64),
                    ]),
                )
                .await?,
        )
        .await?
        {
            let [SqlValue::Blob(peer), SqlValue::Integer(covers)] = record.as_slice() else {
                return Err(Error::Corrupt("peer frontier".into()));
            };
            let peer = peer_id(peer)?;
            held.insert(peer);
            if *covers == 1 {
                applied.insert(peer);
            }
        }
        Ok((held, applied))
    }
    /// One page of frames `request` lacks, after `after`. A requester behind
    /// this replica's horizon gets the signal and no frames.
    pub(crate) async fn range(
        &mut self,
        peer: PublicKey,
        credentials: &CredentialChain,
        request: &ScopeFrontier,
        after: Option<(PublicKey, u64)>,
        byte_quota: usize,
    ) -> Result<RangePage, Error> {
        let page = self
            .range_frames(peer, credentials, request, after, byte_quota)
            .await?;
        // Frames leave the device: they count as offered from here on.
        let hashes = page
            .frames
            .iter()
            .map(EncryptedFrame::id)
            .collect::<Result<Vec<_>, _>>()?;
        self.offer(&hashes).await?;
        Ok(page)
    }
    async fn range_frames(
        &mut self,
        peer: PublicKey,
        credentials: &CredentialChain,
        request: &ScopeFrontier,
        after: Option<(PublicKey, u64)>,
        byte_quota: usize,
    ) -> Result<RangePage, Error> {
        request.validate()?;
        let scope = request.scope;
        self.authorize_peer(peer, credentials, scope)?;
        self.same_tip_peer(peer)?;
        if byte_quota == 0 || byte_quota > MAX_SECURE_FRAME {
            return Err(denied("sync quota"));
        }
        if !self.holds(scope).await? {
            return Err(denied("scope is not held by this replica"));
        }
        let own = self.frontier(scope).await?;
        let mut page = RangePage {
            scope,
            frames: vec![],
            next: None,
            behind: false,
            frontier: own.clone(),
            voids: vec![],
            cuts: self
                .cuts_after(self.tip_log(scope.domain), request.cuts)
                .await?,
        };
        if let Some(id) = self.find_scope_id(scope).await? {
            for (log, horizon) in self.logs(id, None, true, MAX_CHECKPOINT_LOGS + 1).await? {
                if request.held(log.author) as i64 <= horizon {
                    page.behind = true;
                    return Ok(page);
                }
            }
        }
        let blob = encode(&scope)?;
        let mut used = 0;
        // A log stops at a frame of an uncommitted coordinated entry.
        let staged = self.staged().await?;
        for log in &own.logs {
            let mut start = request.held(log.author);
            if let Some((author, sequence)) = after {
                if log.author < author {
                    continue;
                }
                if log.author == author {
                    start = start.max(sequence + 1);
                }
            }
            let records = collect_rows(
                self.conn
                    .query(
                        "SELECT sequence,header,body FROM secure_operations WHERE scope=? AND author=? AND sequence>=? ORDER BY sequence LIMIT ?",
                        bindings(vec![
                            SqlValue::Blob(blob.clone()),
                            SqlValue::Blob(log.author.as_bytes().to_vec()),
                            SqlValue::Integer(start as i64),
                            SqlValue::Integer(RANGE_FRAMES as i64 + 1),
                        ]),
                    )
                    .await?,
            )
            .await?;
            // Voided positions interleave with the log's frames.
            let mut voids = self
                .voids_from(scope, log.author, start, RANGE_FRAMES + 1)
                .await?
                .into_iter()
                .peekable();
            let mut records = records.into_iter().peekable();
            loop {
                let next_frame = match records.peek().map(Vec::as_slice) {
                    Some([SqlValue::Integer(sequence), ..]) => Some(*sequence as u64),
                    Some(_) => return Err(Error::Corrupt("retained operation".into())),
                    None => None,
                };
                if let Some((sequence, _, _)) = voids.peek()
                    && next_frame.is_none_or(|frame| *sequence < frame)
                {
                    let Some((sequence, digest, record)) = voids.next() else {
                        break;
                    };
                    if !page
                        .voids
                        .iter()
                        .any(|held| held.digest().ok() == Some(digest))
                    {
                        let weight = record.weight();
                        if page.frames.len() + page.voids.len() == RANGE_FRAMES
                            || used + weight > byte_quota
                        {
                            if page.frames.is_empty() && page.voids.is_empty() {
                                return Err(denied("byte quota smaller than one frame"));
                            }
                            return Ok(page);
                        }
                        used += weight;
                        page.voids.push(record);
                    }
                    page.next = Some((log.author, sequence));
                    continue;
                }
                let Some(record) = records.next() else {
                    break;
                };
                let [
                    SqlValue::Integer(sequence),
                    SqlValue::Blob(header),
                    SqlValue::Blob(body),
                ] = record.as_slice()
                else {
                    return Err(Error::Corrupt("retained operation".into()));
                };
                let frame = self.stored_frame(header.clone(), body.clone()).await?;
                if staged.contains(&frame.id()?) {
                    break;
                }

                // Every frame of a revoked author is refused, so none is
                // sent.
                if !self.visible_in_workspace(&frame.operation()?)?
                    || self
                        .config
                        .revocations
                        .revoking(frame.chain()?.credentials())
                        .map_err(credential)?
                        .is_some()
                {
                    break;
                }
                let weight = frame.weight();
                if page.frames.len() + page.voids.len() == RANGE_FRAMES
                    || used + weight > byte_quota
                {
                    if page.frames.is_empty() && page.voids.is_empty() {
                        return Err(denied("byte quota smaller than one frame"));
                    }
                    return Ok(page);
                }
                used += weight;
                page.next = Some((log.author, *sequence as u64));
                page.frames.push(frame);
            }
        }
        page.next = None;
        Ok(page)
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// Declares this replica's role and the full replica nodes it relies on.
    pub async fn set_role(&self, config: RoleConfig) -> Result<(), Error> {
        let mut inner = self.inner.lock().await;
        if config.full_nodes.len() > 64
            || config.custody > config.full_nodes.len()
            || config
                .full_nodes
                .contains(&PeerId::of(inner.config.node.public_key()))
        {
            return Err(denied("invalid role configuration"));
        }
        let evicting = rows(
            &inner.conn,
            "SELECT 1 FROM secure_subscriptions WHERE state='evicting' LIMIT 1",
        )
        .await?;
        if config.role == ReplicaRole::Full && !evicting.is_empty() {
            return Err(denied("finish the eviction in progress first"));
        }
        let bytes = encode(&config)?;
        transaction!(inner, |inner| {
            inner
                .conn
                .execute(
                    "INSERT INTO secure_roles VALUES(1,?) ON CONFLICT(id) DO UPDATE SET config=excluded.config",
                    [bytes.clone()],
                )
                .await?;
            if config.role == ReplicaRole::Full {
                // A full replica holds every scope again.
                inner
                    .conn
                    .execute(
                        "UPDATE secure_subscriptions SET state='held' WHERE state='evicted'",
                        (),
                    )
                    .await?;
            }
            Ok(())
        })
    }
    pub async fn role(&self) -> Result<RoleConfig, Error> {
        self.inner.lock().await.role().await
    }
    /// The scope's recorded holding; `None` until it was subscribed, left
    /// or evicted.
    pub async fn subscription(&self, scope: ResourceScope) -> Result<Option<Holding>, Error> {
        self.inner.lock().await.holding(scope).await
    }
    /// Holds the scope from now on; an evicted scope comes back empty and
    /// fills by sync or rebootstrap.
    pub async fn subscribe(&self, scope: ResourceScope) -> Result<(), Error> {
        let mut inner = self.inner.lock().await;
        if !inner.local_grants(now()?)?.iter().any(|grant| {
            grant.scope == scope
                && grant.rows == RowSelection::All
                && (grant.rights.contains(&Right::Read) || grant.rights.contains(&Right::Custody))
        }) {
            return Err(denied("subscription needs whole-scope custody or read"));
        }
        if inner.holding(scope).await? == Some(Holding::Evicting) {
            return Err(denied("finish the eviction in progress first"));
        }
        transaction!(inner, |inner| inner.set_holding(scope, Holding::Held).await)
    }
    /// Stops holding new frames of the scope. Rows already held stay; they
    /// are neither deleted nor reported complete.
    pub async fn unsubscribe(&self, scope: ResourceScope) -> Result<(), Error> {
        let mut inner = self.inner.lock().await;
        match inner.holding(scope).await? {
            Some(Holding::Evicting | Holding::Evicted) => Ok(()),
            _ => transaction!(inner, |inner| inner.set_holding(scope, Holding::Left).await),
        }
    }
    /// Ends the restoration of a resubscribed scope after a complete
    /// catch-up from a peer, once the own log an eviction dropped is back.
    pub(crate) async fn restored(&self, scope: ResourceScope) -> Result<(), Error> {
        let mut inner = self.inner.lock().await;
        if inner.holding(scope).await? != Some(Holding::Held) {
            return Ok(());
        }
        let Some(guard) = inner.eviction_guard(scope).await? else {
            return Ok(());
        };
        let tip = match inner.find_scope_id(scope).await? {
            Some(id) => inner
                .log(id, inner.config.node.public_key())
                .await?
                .map(|log| log.sequence),
            None => None,
        };
        if tip.unwrap_or(-1) < guard {
            return Ok(());
        }
        transaction!(inner, |inner| {
            inner
                .conn
                .execute(
                    "DELETE FROM secure_evicted_logs WHERE scope=?",
                    [encode(&scope)?],
                )
                .await?;
            inner
                .conn
                .execute(
                    "UPDATE secure_metadata SET revision=revision+1 WHERE id=1",
                    (),
                )
                .await?;
            Ok(())
        })?;
        inner.publish_revision().await
    }
    /// Scopes with a recorded subscription state, in scope order.
    pub async fn subscriptions(&self) -> Result<Vec<(ResourceScope, Holding)>, Error> {
        let inner = self.inner.lock().await;
        let mut scopes = Vec::new();
        for record in rows(
            &inner.conn,
            "SELECT scope,state FROM secure_subscriptions ORDER BY scope",
        )
        .await?
        {
            let [SqlValue::Blob(scope), SqlValue::Text(state)] = record.as_slice() else {
                return Err(Error::Corrupt("subscription".into()));
            };
            scopes.push((decode(scope)?, Holding::parse(state)?));
        }
        scopes.sort_by_key(|(scope, _)| *scope);
        Ok(scopes)
    }
    /// Scopes held here that `peer` may hold too, after `after`, at most 64.
    pub async fn held_scopes(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        after: Option<ResourceScope>,
    ) -> Result<(Vec<ResourceScope>, Option<ResourceScope>), Error> {
        self.held_scopes_proved(peer, credentials, after, None)
            .await
    }
    pub(crate) async fn held_scopes_proved(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        after: Option<ResourceScope>,
        proved: Option<&BTreeSet<ResourceScope>>,
    ) -> Result<(Vec<ResourceScope>, Option<ResourceScope>), Error> {
        let inner = self.inner.lock().await;
        let grants = inner.peer_grants(peer, credentials)?;
        let mut scopes = BTreeSet::new();
        for sql in [
            "SELECT scope FROM secure_scope_state",
            "SELECT scope FROM secure_subscriptions",
        ] {
            for record in rows(&inner.conn, sql).await? {
                let [SqlValue::Blob(scope)] = record.as_slice() else {
                    return Err(Error::Corrupt("scope".into()));
                };
                scopes.insert(decode::<ResourceScope>(scope)?);
            }
        }
        let mut held = Vec::new();
        for scope in scopes {
            if proved.is_some_and(|proved| inner.partitioned(scope) && !proved.contains(&scope))
                || after.is_some_and(|after| scope <= after)
                || !inner.holds(scope).await?
            {
                continue;
            }
            if grants.iter().any(|grant| {
                grant.scope == scope
                    && grant.rows == RowSelection::All
                    && (grant.rights.contains(&Right::Read)
                        || grant.rights.contains(&Right::Custody))
            }) {
                if held.len() == 64 {
                    let next = held.last().copied();
                    return Ok((held, next));
                }
                held.push(scope);
            }
        }
        Ok((held, None))
    }
    /// The contiguous prefix of every log of `scope` this replica holds.
    pub async fn frontier(&self, scope: ResourceScope) -> Result<ScopeFrontier, Error> {
        self.inner.lock().await.frontier(scope).await
    }
    /// Frames `request` lacks, at most 32 and `byte_quota` bytes, after
    /// `after`; or the signal that the peer must rebootstrap.
    pub async fn range_page(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        request: &ScopeFrontier,
        after: Option<(PublicKey, u64)>,
        byte_quota: usize,
    ) -> Result<RangePage, Error> {
        let (page, commit, flusher) = {
            let mut inner = self.inner.lock().await;
            let page = inner
                .range(peer, credentials, request, after, byte_quota)
                .await?;
            (page, inner.commit, inner.flusher.clone())
        };
        // Frames leave the device: flush every commit they may include,
        // outside the lock so local writes continue meanwhile.
        flusher.flush(commit)?;
        Ok(page)
    }
    /// Records a peer's frontier as acknowledgment of everything below it.
    /// Its applied part counts as custody only from a whole-scope reader.
    pub async fn acknowledge_frontier(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        frontier: &ScopeFrontier,
    ) -> Result<(), Error> {
        self.inner
            .lock()
            .await
            .record_frontier(peer, credentials, frontier)
            .await
    }
    /// Fails unless `peer` may hold the whole scope.
    pub(crate) async fn authorize_scope_peer(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scope: ResourceScope,
    ) -> Result<(), Error> {
        self.inner
            .lock()
            .await
            .authorize_peer(peer, credentials, scope)
    }
    /// Role, holding, retained window, custody coverage and peer lag of
    /// every known scope.
    pub async fn replica_status(&self) -> Result<ReplicaStatus, Error> {
        let inner = self.inner.lock().await;
        let role = inner.role().await?;
        let mut scopes = BTreeSet::new();
        for sql in [
            "SELECT scope FROM secure_scope_state",
            "SELECT scope FROM secure_subscriptions",
        ] {
            for record in rows(&inner.conn, sql).await? {
                let [SqlValue::Blob(scope)] = record.as_slice() else {
                    return Err(Error::Corrupt("scope".into()));
                };
                scopes.insert(decode::<ResourceScope>(scope)?);
            }
        }
        let me = inner.config.node.public_key();
        let mut status = Vec::new();
        for scope in scopes {
            let holding = match inner.holding(scope).await? {
                Some(holding) => holding,
                None if role.role == ReplicaRole::Full => Holding::Held,
                None => Holding::Left,
            };
            let blob = encode(&scope)?;
            let frontier = inner.frontier(scope).await?;
            let mut entry = ScopeStatus {
                scope,
                holding,
                rows: 0,
                covered: 0,
                uncovered: 0,
                pruned_logs: 0,
                local_operations: frontier.log(me).map_or(0, |log| log.applied),
                custody: vec![],
                peers: vec![],
            };
            if let Some(id) = inner.find_scope_id(scope).await? {
                let (rows, tombstones, _, _) = inner.scope_state(id).await?;
                entry.rows = rows - tombstones;
                let position = inner
                    .checkpoint_record(id, true)
                    .await?
                    .map_or(0, |record| record.position);
                let counts = collect_rows(
                    inner
                        .conn
                        .query(
                            "SELECT COALESCE(SUM(position<=?),0),COALESCE(SUM(position>?),0) FROM secure_applied WHERE scope=?",
                            [position, position, id],
                        )
                        .await?,
                )
                .await?;
                if let Some([SqlValue::Integer(covered), SqlValue::Integer(uncovered)]) =
                    counts.first().map(Vec::as_slice)
                {
                    entry.covered = *covered as u64;
                    entry.uncovered = *uncovered as u64;
                }
                entry.pruned_logs = inner.logs(id, None, true, MAX_CHECKPOINT_LOGS).await?.len();
            }
            for node in &role.full_nodes {
                let (_, applied) = inner.peer_mark(*node, &blob, me).await?;
                entry
                    .custody
                    .push((*node, applied.min(entry.local_operations)));
            }
            let peers = collect_rows(
                inner
                    .conn
                    .query(
                        "SELECT DISTINCT peer FROM secure_peer_frontiers WHERE scope=? ORDER BY peer",
                        [blob.clone()],
                    )
                    .await?,
            )
            .await?;
            for record in peers {
                let [SqlValue::Blob(peer)] = record.as_slice() else {
                    return Err(Error::Corrupt("peer frontier".into()));
                };
                let peer = peer_id(peer)?;
                let mut theirs = ScopeFrontier {
                    scope,
                    logs: vec![],
                    cuts: 0,
                };
                for log in collect_rows(
                    inner
                        .conn
                        .query(
                            "SELECT author,held,applied FROM secure_peer_frontiers WHERE peer=? AND scope=? ORDER BY author",
                            [peer.as_bytes().to_vec(), blob.clone()],
                        )
                        .await?,
                )
                .await?
                {
                    let [
                        SqlValue::Blob(author),
                        SqlValue::Integer(held),
                        SqlValue::Integer(applied),
                    ] = log.as_slice()
                    else {
                        return Err(Error::Corrupt("peer frontier".into()));
                    };
                    theirs.logs.push(FrontierLog {
                        author: key_of(author)?,
                        held: *held as u64,
                        applied: *applied as u64,
                    });
                }
                entry.peers.push(PeerLag {
                    peer,
                    behind: theirs.lag_behind(&frontier),
                    ahead: frontier.lag_behind(&theirs),
                });
            }
            status.push(entry);
        }
        Ok(ReplicaStatus {
            role,
            scopes: status,
        })
    }
}
impl<A: RowApplication> EncryptedReplica<A> {
    pub(crate) async fn exchange_frontiers(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        frontiers: &[ScopeFrontier],
    ) -> Result<Vec<FrontierReply>, Error> {
        validate_frontiers(frontiers)?;
        for frontier in frontiers {
            frontier.validate()?;
        }
        let mut inner = self.inner.lock().await;
        if peer == inner.config.node.public_key() {
            return Err(denied("a replica cannot acknowledge itself"));
        }
        // Verify the entire request before publishing any scope's metadata.
        let scopes: Vec<_> = frontiers.iter().map(|frontier| frontier.scope).collect();
        let grants = inner.verify_chain_scopes(credentials, peer, now()?, &scopes)?;
        let held: Vec<_> = frontiers
            .iter()
            .filter(|frontier| holds_whole(&grants, frontier.scope))
            .cloned()
            .collect();
        inner.store_frontiers(peer, &grants, &held).await?;
        let mut replies = Vec::new();
        for frontier in frontiers {
            if !holds_whole(&grants, frontier.scope) {
                replies.push(FrontierReply::Refused(frontier.scope));
                continue;
            }
            replies.push(FrontierReply::Held(FrontierState {
                frontier: inner.frontier(frontier.scope).await?,
                signed: inner.mergeable_horizon(frontier.scope).await?,
                behind: inner.horizon_device_behind(frontier.scope, peer).await?
                    || inner
                        .horizon_device_behind(frontier.scope, inner.config.node.public_key())
                        .await?,
                importing: inner.importing(frontier.scope).await?,
            }));
        }
        Ok(replies)
    }
    /// The members of `scopes` that `peer` may hold whole, in order, so a
    /// client discloses no frontier to a peer outside a scope. A chain this
    /// replica cannot verify, as of a newer generation, is not filtered.
    pub(crate) async fn peer_holds(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scopes: &[ResourceScope],
    ) -> Result<Vec<ResourceScope>, Error> {
        let inner = self.inner.lock().await;
        if inner.peer_tip(peer) != Some(inner.tip) {
            return Ok(Vec::new());
        }
        let grants = inner.verify_chain_scopes(credentials, peer, now()?, scopes)?;
        Ok(scopes
            .iter()
            .copied()
            .filter(|scope| holds_whole(&grants, *scope))
            .collect())
    }
}

pub(super) fn peer_id(bytes: &[u8]) -> Result<PeerId, Error> {
    bytes
        .try_into()
        .map(PeerId::from_bytes)
        .map_err(|_| Error::Corrupt("peer reference".into()))
}
