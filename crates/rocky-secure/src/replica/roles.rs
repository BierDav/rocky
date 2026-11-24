use super::checkpoint::MAX_CHECKPOINT_LOGS;
use super::*;
use rocky_credentials::PeerId;
mod restoration;

const PEER_FRONTIERS_LOG_INDEX: &str = "CREATE INDEX IF NOT EXISTS secure_peer_frontiers_log ON secure_peer_frontiers(scope,author,held)";
pub(crate) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_roles(id INTEGER PRIMARY KEY CHECK(id=1),config BLOB NOT NULL)",
    "CREATE TABLE IF NOT EXISTS secure_subscriptions(scope BLOB PRIMARY KEY) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_restoration_guards(scope BLOB PRIMARY KEY,sequence INTEGER NOT NULL,hash BLOB NOT NULL)",
    "CREATE TABLE IF NOT EXISTS secure_peer_frontiers(peer BLOB NOT NULL,scope BLOB NOT NULL,author BLOB NOT NULL,held INTEGER NOT NULL,applied INTEGER NOT NULL,PRIMARY KEY(peer,scope,author)) WITHOUT ROWID",
    PEER_FRONTIERS_LOG_INDEX,
    "CREATE INDEX IF NOT EXISTS secure_operations_scope ON secure_operations(scope,author,sequence)",
];

pub(crate) const RANGE_FRAMES: usize = 32;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum ReplicaRole {
    #[default]
    Full,
    Partial,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoleConfig {
    pub role: ReplicaRole,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FrontierLog {
    pub author: PublicKey,
    pub held: u64,
    pub applied: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScopeFrontier {
    pub scope: ResourceScope,
    pub logs: Vec<FrontierLog>,
}
impl ScopeFrontier {
    pub fn log(&self, author: PublicKey) -> Option<&FrontierLog> {
        self.logs.iter().find(|log| log.author == author)
    }
    fn held(&self, author: PublicKey) -> u64 {
        self.log(author).map_or(0, |log| log.held)
    }
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
            || self
                .logs
                .iter()
                .any(|log| log.applied > log.held || log.held > i64::MAX as u64)
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
    pub(crate) importing: bool,
}
#[derive(Serialize, Deserialize)]
pub(crate) enum FrontierReply {
    Held(FrontierState),
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StateAdvice {
    pub seal: Commitment,
    pub checkpoint: Commitment,
    pub signer: PublicKey,
    pub rows: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RangePage {
    pub state: Option<StateAdvice>,
    pub scope: ResourceScope,
    pub frames: Vec<EncryptedFrame>,
    pub next: Option<(PublicKey, u64)>,
    pub behind: bool,
    pub frontier: ScopeFrontier,
    pub voids: Vec<SignedVoid>,
}

impl<A: RowApplication> Inner<A> {
    pub(crate) async fn role(&self) -> Result<RoleConfig, Error> {
        if let Some(role) = self.metadata.with(|cache| cache.role.clone()) {
            return Ok(role);
        }
        let stored = rows(&self.conn, "SELECT config FROM secure_roles WHERE id=1").await?;
        let role = match stored.first().map(Vec::as_slice) {
            Some([SqlValue::Blob(bytes)]) => decode(bytes)?,
            None => RoleConfig::default(),
            _ => return Err(Error::Corrupt("role configuration".into())),
        };
        self.metadata.with(|cache| cache.role = Some(role.clone()));
        Ok(role)
    }
    pub(crate) async fn subscribed(&self, scope: ResourceScope) -> Result<bool, Error> {
        if let Some(subscribed) = self
            .metadata
            .with(|cache| cache.subscriptions.get(&scope).copied())
        {
            return Ok(subscribed);
        }
        let mut result = self
            .conn
            .query(
                "SELECT 1 FROM secure_subscriptions WHERE scope=?",
                [encode(&scope)?],
            )
            .await?;
        let subscribed = result.next().await?.is_some();
        self.metadata.with(|cache| {
            cache.subscriptions.insert(scope, subscribed);
        });
        Ok(subscribed)
    }
    pub(super) fn custody_scopes(&self, time: u64) -> Result<BTreeSet<ResourceScope>, Error> {
        let grants = self.local_grants(time)?;
        Ok(grants
            .iter()
            .filter(|grant| holds_whole(std::slice::from_ref(*grant), grant.scope))
            .map(|grant| grant.scope)
            .collect())
    }
    pub(crate) async fn holds(&self, scope: ResourceScope) -> Result<bool, Error> {
        Ok(self.subscribed(scope).await? || self.role().await?.role == ReplicaRole::Full)
    }
    async fn restoration_guard(
        &self,
        scope: ResourceScope,
    ) -> Result<Option<(i64, Option<Hash>)>, Error> {
        let mut result = self
            .conn
            .query(
                "SELECT sequence,hash FROM secure_restoration_guards WHERE scope=?",
                [encode(&scope)?],
            )
            .await?;
        let Some(row) = result.next().await? else {
            return Ok(None);
        };
        let sequence = row.get::<i64>(0)?;
        let hash = row.get::<Vec<u8>>(1)?;
        match (sequence, hash.is_empty()) {
            (-1, true) => Ok(Some((-1, None))),
            (sequence, false) if sequence >= 0 => {
                Ok(Some((sequence, Some(checkpoint::hash_of(&hash)?))))
            }
            _ => Err(Error::Corrupt("restoration guard".into())),
        }
    }
    pub(crate) async fn admit_scope(
        &self,
        header: &p2panda_core::Header<Extensions>,
    ) -> Result<(), Error> {
        let scope = header.extensions.scope;
        if !self.holds(scope).await? {
            return Err(denied("scope is not held by this replica"));
        }
        if header.public_key == self.config.node.public_key() {
            let adoptable = match self.restoration_guard(scope).await? {
                Some((sequence, hash)) => {
                    let at = header.seq_num as i64;
                    at < sequence || (at == sequence && hash == Some(header.hash()))
                }
                None => header.seq_num < self.reported_own_log(scope, header.public_key).await?,
            };
            if !adoptable {
                return Err(denied("unknown operation from local signing identity"));
            }
            self.raise_author_sequence(header.public_key, header.extensions.author_sequence)
                .await?;
        }
        Ok(())
    }
    async fn restoration_complete(
        &self,
        scope: ResourceScope,
        (sequence, hash): (i64, Option<Hash>),
    ) -> Result<bool, Error> {
        let Some(hash) = hash else { return Ok(true) };
        let Some(id) = self.find_scope_id(scope).await? else {
            return Ok(false);
        };
        let node = self.config.node.public_key();
        let log = self.log(id, node).await?;
        if let Some(log) = &log {
            if log.sequence == sequence && log.hash == hash {
                return Ok(true);
            }
            if log.sequence > sequence
                && self
                    .applied_restoration_endpoint(scope, id, sequence, hash)
                    .await?
            {
                return Ok(true);
            }
        }
        self.restoration_dispositions(scope, id, sequence, hash, log)
            .await
    }
    pub(super) async fn own_chain_ready(
        &self,
        header: &p2panda_core::Header<Extensions>,
        bundle: &BTreeSet<Hash>,
    ) -> Result<bool, Error> {
        let scope = header.extensions.scope;
        if header.public_key != self.config.node.public_key()
            || self.restoration_guard(scope).await?.is_none()
        {
            return Ok(true);
        }
        let log = match self.find_scope_id(scope).await? {
            Some(id) => self.log(id, header.public_key).await?,
            None => None,
        };
        Ok(match (log, header.backlink) {
            (_, Some(backlink)) if bundle.contains(&backlink) => true,
            (Some(log), Some(backlink)) => {
                log.sequence + 1 == header.seq_num as i64 && log.hash == backlink
            }
            (None, None) => header.seq_num == 0,
            _ => false,
        })
    }
    pub(crate) async fn admit_author(
        &self,
        scope: ResourceScope,
        author: PublicKey,
    ) -> Result<(), Error> {
        if !self.holds(scope).await? {
            return Err(denied("author needs a held scope"));
        }
        if let Some(guard) = self.restoration_guard(scope).await? {
            if !self.restoration_complete(scope, guard).await? {
                return Err(denied(
                    "catch up the resubscribed scope before authoring in it",
                ));
            }
            self.conn
                .execute(
                    "DELETE FROM secure_restoration_guards WHERE scope=?",
                    [encode(&scope)?],
                )
                .await?;
        }
        self.admit_lane(scope, author).await
    }
    pub(super) async fn merge_restoration_guard(
        &self,
        scope: ResourceScope,
        endpoint: (i64, Option<Hash>),
    ) -> Result<(), Error> {
        if let Some((held, hash)) = self.restoration_guard(scope).await? {
            if held > endpoint.0 {
                return Ok(());
            }
            if held == endpoint.0 {
                if hash != endpoint.1 {
                    return Err(Error::Corrupt("conflicting restoration endpoints".into()));
                }
                return Ok(());
            }
        }
        self.conn
            .execute(
                "INSERT INTO secure_restoration_guards VALUES(?,?,?) ON CONFLICT(scope) DO UPDATE SET sequence=excluded.sequence,hash=excluded.hash",
                bindings(vec![
                    SqlValue::Blob(encode(&scope)?),
                    SqlValue::Integer(endpoint.0),
                    SqlValue::Blob(endpoint.1.map_or_else(Vec::new, |hash| hash.as_bytes().to_vec())),
                ]),
            )
            .await?;
        Ok(())
    }
    pub(super) async fn save_restoration_guard(&self, scope: ResourceScope) -> Result<(), Error> {
        let log = match self.find_scope_id(scope).await? {
            Some(id) => self.log(id, self.config.node.public_key()).await?,
            None => None,
        };
        self.merge_restoration_guard(
            scope,
            log.map_or((-1, None), |log| (log.sequence, Some(log.hash))),
        )
        .await
    }
    pub(crate) async fn restoring(&self, scope: ResourceScope) -> Result<bool, Error> {
        Ok(self.restoration_guard(scope).await?.is_some())
    }
    pub(crate) async fn frontier(&self, scope: ResourceScope) -> Result<ScopeFrontier, Error> {
        self.tip_frontier(scope).await
    }

    fn peer_grants(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
    ) -> Result<Vec<rocky_core::ResourceGrant>, Error> {
        self.verify_chain(credentials, peer, now()?)
    }
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
        let scopes: Vec<_> = frontiers.iter().map(|frontier| frontier.scope).collect();
        if self.peer_tip(peer) != Some(self.workspace.tip) {
            let Ok(grants) = self.verify_chain_scopes(credentials, peer, now()?, &scopes) else {
                return Ok(());
            };
            return self.record_lane_reports(&grants, frontiers).await;
        }
        let grants = self.verify_chain_scopes(credentials, peer, now()?, &scopes)?;
        self.store_frontiers(peer, &grants, frontiers).await
    }
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
            reports.push((frontier, encode(&frontier.scope)?, reads));
        }
        let recorded = reports
            .iter()
            .any(|(frontier, ..)| !frontier.logs.is_empty());
        self.known_peers.acknowledged(PeerId::of(peer), recorded);
        let first = recorded && self.unrecorded_peer(peer).await?;
        transaction!(@keeping_frontier_cache self, |inner| {
            for (frontier, scope, reads) in &reports {
                let regressed = inner.forget_regressed_peer(peer, frontier).await?;
                for log in &frontier.logs {
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
                if regressed {
                    inner.retract_lost_acknowledgments(peer, frontier.scope).await?;
                }
                inner.record_own_reports(frontier).await?;
            }
            let scopes: Vec<_> = frontiers.iter().map(|frontier| frontier.scope).collect();
            inner.confirm_scopes(&scopes).await?;
            if first {
                inner.bound_peers().await?;
            }
            Ok(())
        })?;
        Ok(())
    }
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
    pub(crate) async fn range(
        &mut self,
        peer: PublicKey,
        credentials: &CredentialChain,
        request: &ScopeFrontier,
        after: Option<(PublicKey, u64)>,
        byte_quota: usize,
    ) -> Result<RangePage, Error> {
        let mut page = self
            .range_frames(peer, credentials, request, after, byte_quota, None)
            .await?;
        if !page.behind
            && let Some(signed) = self
                .offered_checkpoint(peer, credentials, request.scope)
                .await?
        {
            let base = &signed.statement;
            if let Some(seal) = base.seal
                && self.path_index(request.scope.domain, seal)?.is_some()
                && signed.signer() != peer
                && base_covers(request, &base.frontier)
                && self.lacking_base_frames(request, &base.frontier).await? >= 64
            {
                page.state = Some(StateAdvice {
                    seal,
                    checkpoint: signed.id()?,
                    signer: signed.signer(),
                    rows: base.rows.count,
                });
            }
        }
        let hashes = page
            .frames
            .iter()
            .map(EncryptedFrame::id)
            .collect::<Result<Vec<_>, _>>()?;
        self.record_offered_before_sending(&hashes).await?;
        Ok(page)
    }
    async fn lacking_base_frames(
        &self,
        request: &ScopeFrontier,
        base: &[super::checkpoint::LogPosition],
    ) -> Result<u64, Error> {
        let mut missing = 0u64;
        for log in base {
            let records = collect_rows(self.conn.query(
                "SELECT count(*) FROM (SELECT 1 FROM secure_operations WHERE scope=? AND author=? AND sequence>=? AND sequence<=? LIMIT 64)",
                bindings(vec![SqlValue::Blob(encode(&request.scope)?), SqlValue::Blob(log.author.as_bytes().to_vec()),
                    SqlValue::Integer(request.held(log.author) as i64), SqlValue::Integer(log.sequence as i64)]),
            ).await?).await?;
            let [SqlValue::Integer(count)] = records[0].as_slice() else {
                return Err(Error::Corrupt("covered frames".into()));
            };
            missing += *count as u64;
            if missing >= 64 {
                break;
            }
        }
        Ok(missing)
    }

    pub(super) async fn range_frames(
        &mut self,
        peer: PublicKey,
        credentials: &CredentialChain,
        request: &ScopeFrontier,
        after: Option<(PublicKey, u64)>,
        byte_quota: usize,
        upper: Option<&ScopeFrontier>,
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
        let own = match upper {
            Some(upper) if upper.scope == scope => upper.clone(),
            Some(_) => return Err(denied("range upper scope")),
            None => self.frontier(scope).await?,
        };
        let mut page = RangePage {
            state: None,
            scope,
            frames: vec![],
            next: None,
            behind: false,
            frontier: own.clone(),
            voids: vec![],
        };
        if let Some(id) = self.find_scope_id(scope).await? {
            for (log, pruned_prefix) in self.logs(id, None, true, MAX_CHECKPOINT_LOGS + 1).await? {
                if request.held(log.author) as i64 <= pruned_prefix {
                    page.behind = true;
                    return Ok(page);
                }
            }
        }
        let blob = encode(&scope)?;
        let mut used = 0;
        let staged = self.unsent().await?;
        for log in &own.logs {
            let mut start = request.held(log.author);
            if let Some((author, sequence)) = after {
                if log.author < author {
                    continue;
                }
                if log.author == author {
                    let next = sequence
                        .checked_add(1)
                        .filter(|next| *next <= i64::MAX as u64)
                        .ok_or_else(|| denied("range position"))?;
                    start = start.max(next);
                }
            }
            let records = collect_rows(
                self.conn
                    .query(
                        "SELECT sequence,header,body FROM secure_operations WHERE scope=? AND author=? AND sequence>=? AND sequence<? ORDER BY sequence LIMIT ?",
                        bindings(vec![
                            SqlValue::Blob(blob.clone()),
                            SqlValue::Blob(log.author.as_bytes().to_vec()),
                            SqlValue::Integer(start as i64),
                            SqlValue::Integer(if upper.is_some() { i64::try_from(log.held).map_err(|_| denied("range upper position"))? } else { i64::MAX }),
                            SqlValue::Integer(RANGE_FRAMES as i64 + 1),
                        ]),
                    )
                    .await?,
            )
            .await?;
            let mut voids = self
                .voids_from(scope, log.author, start, RANGE_FRAMES + 1)
                .await?
                .into_iter()
                .take_while(|(sequence, _, _)| upper.is_none() || *sequence < log.held)
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

                let operation = frame.operation_ref()?;
                if !self.visible_in_workspace(operation)?
                    || !self.offered(&operation.header).await?
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
    pub async fn set_role(&self, config: RoleConfig) -> Result<(), Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        let bytes = encode(&config)?;
        transaction!(inner, |inner| {
            inner
                .conn
                .execute(
                    "INSERT INTO secure_roles VALUES(1,?) ON CONFLICT(id) DO UPDATE SET config=excluded.config",
                    [bytes.clone()],
                )
                .await?;
            Ok(())
        })
    }
    pub async fn role(&self) -> Result<RoleConfig, Error> {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .role()
            .await
    }
    pub async fn subscribed(&self, scope: ResourceScope) -> Result<bool, Error> {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .subscribed(scope)
            .await
    }
    pub async fn subscribe(&self, scope: ResourceScope) -> Result<(), Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        if !inner.local_grants(now()?)?.iter().any(|grant| {
            grant.scope == scope
                && grant.rows == RowSelection::All
                && (grant.rights.contains(&Right::Read) || grant.rights.contains(&Right::Custody))
        }) {
            return Err(denied("subscription needs whole-scope custody or read"));
        }
        transaction!(inner, |inner| {
            inner
                .conn
                .execute(
                    "INSERT OR IGNORE INTO secure_subscriptions VALUES(?)",
                    [encode(&scope)?],
                )
                .await?;
            Ok(())
        })
    }
    #[cfg(test)]
    pub(crate) async fn drop_local_history_for_test(&self, scope: ResourceScope) -> u64 {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        let blob = encode(&scope).unwrap();
        let id = inner.find_scope_id(scope).await.unwrap();
        let dropped = transaction!(inner, |inner| {
            inner.save_restoration_guard(scope).await?;
            inner
                .replace_ledger(scope, super::identity::IdentityLedger::default())
                .await?;
            if inner.replayed_scope(scope) {
                inner.clear_replayed(scope).await?;
            }
            let operations = collect_rows(
                inner
                    .conn
                    .query(
                        "SELECT hash,LENGTH(header)+LENGTH(body) FROM secure_operations WHERE scope=?",
                        [blob.clone()],
                    )
                    .await?,
            )
            .await?;
            let mut bytes = 0;
            for record in &operations {
                let [SqlValue::Blob(hash), SqlValue::Integer(size)] = record.as_slice() else {
                    return Err(Error::Corrupt("retained operation".into()));
                };
                for table in [
                    "secure_operations",
                    "secure_outbox",
                    "secure_unoffered",
                    "secure_ack",
                    "secure_applied",
                    "secure_publication_frames",
                ] {
                    inner
                        .conn
                        .execute(format!("DELETE FROM {table} WHERE hash=?"), [hash.clone()])
                        .await?;
                }
                bytes += *size;
            }
            inner.clear_applied_cache();
            inner.sweep_chains().await?;
            inner
                .conn
                .execute(
                    "UPDATE secure_retained SET records=records-?,bytes=bytes-?",
                    [operations.len() as i64, bytes],
                )
                .await?;
            inner
                .conn
                .execute("DELETE FROM secure_row_records WHERE scope=?", [blob.clone()])
                .await?;
            if let Some(id) = id {
                for table in [
                    "secure_checkpoint_base",
                    "secure_checkpoint_overlay",
                    "secure_checkpoint_scratch",
                    "secure_rebootstrap_rows",
                    "secure_seal_anchors",
                    "secure_applied",
                    "secure_stubs",
                    "secure_logs",
                    "secure_checkpoints",
                    "secure_rebootstrap",
                    "secure_rebootstrap_stubs",
                    "secure_rebootstrap_pages",
                    "secure_checkpoint_pages",
                    "secure_checkpoint_stubs",
                    "secure_compaction_live_creations",
                    "secure_seal_bounds",
                    "secure_seal_drops",
                    "secure_replayed_seals",
                    "secure_replayed_seal_rows",
                    "secure_replayed_snapshots",
                    "secure_replayed_base_source",
                    "secure_replayed_base_rows",
                    "secure_base_transition",
                    "secure_base_cleanup",
                    "secure_replayed_staged",
                ] {
                    inner
                        .conn
                        .execute(format!("DELETE FROM {table} WHERE scope=?"), [id])
                        .await?;
                }
                inner
                    .store_scope_state(id, 0, 0, &super::checkpoint::LtHash16::new(), 0)
                    .await?;
            }
            for table in ["secure_retention", "secure_peer_frontiers", "secure_identity_ledger"] {
                inner
                    .conn
                    .execute(format!("DELETE FROM {table} WHERE scope=?"), [blob.clone()])
                    .await?;
            }
            inner
                .conn
                .execute(
                    "UPDATE secure_metadata SET revision=revision+1 WHERE id=1",
                    (),
                )
                .await?;
            Ok(operations.len() as u64)
        })
        .unwrap();
        if let Some(id) = id {
            inner.verified_at.remove(&id);
        }
        inner.publish_revision().await.unwrap();
        inner.load_direct().await.unwrap();
        inner.retry_rederive(scope.domain).await.unwrap();
        dropped
    }
    #[cfg(test)]
    pub(crate) async fn merge_restoration_for_test(
        &self,
        scope: ResourceScope,
        endpoint: (i64, Option<Hash>),
    ) -> Result<Option<(i64, Option<Hash>)>, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        inner.merge_restoration_guard(scope, endpoint).await?;
        inner.restoration_guard(scope).await
    }
    pub(crate) async fn restored(&self, scope: ResourceScope) -> Result<(), Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        if !inner.holds(scope).await? {
            return Ok(());
        }
        let cleared = transaction!(inner, |inner| {
            let Some(guard) = inner.restoration_guard(scope).await? else {
                return Ok(false);
            };
            if !inner.restoration_complete(scope, guard).await? {
                return Ok(false);
            }
            inner
                .conn
                .execute(
                    "DELETE FROM secure_restoration_guards WHERE scope=?",
                    [encode(&scope)?],
                )
                .await?;
            inner.settle_seal_rebootstrap(scope.domain).await?;
            inner
                .conn
                .execute(
                    "UPDATE secure_metadata SET revision=revision+1 WHERE id=1",
                    (),
                )
                .await?;
            Ok(true)
        })?;
        if !cleared {
            return Ok(());
        }
        inner.publish_revision().await
    }
    pub async fn subscriptions(&self) -> Result<BTreeSet<ResourceScope>, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        let mut scopes = BTreeSet::new();
        for record in rows(&inner.conn, "SELECT scope FROM secure_subscriptions").await? {
            let [SqlValue::Blob(scope)] = record.as_slice() else {
                return Err(Error::Corrupt("subscription".into()));
            };
            scopes.insert(decode(scope)?);
        }
        Ok(scopes)
    }
    pub async fn held_scopes(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        after: Option<ResourceScope>,
    ) -> Result<(Vec<ResourceScope>, Option<ResourceScope>), Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
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
            if after.is_some_and(|after| scope <= after) || !inner.holds(scope).await? {
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
    pub async fn frontier(&self, scope: ResourceScope) -> Result<ScopeFrontier, Error> {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .frontier(scope)
            .await
    }
    pub async fn range_page(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        request: &ScopeFrontier,
        after: Option<(PublicKey, u64)>,
        byte_quota: usize,
    ) -> Result<RangePage, Error> {
        let (page, commit, flusher) = {
            let mut inner = self.inner.lock(crate::replica::Class::Background).await;
            let page = inner
                .range(peer, credentials, request, after, byte_quota)
                .await?;
            (page, inner.locked_commit(), inner.flusher.clone())
        };
        flusher.run(commit).await?;
        Ok(page)
    }
    pub async fn acknowledge_frontier(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        frontier: &ScopeFrontier,
    ) -> Result<(), Error> {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .record_frontier(peer, credentials, frontier)
            .await
    }
    pub(crate) async fn authorize_scope_peer(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scope: ResourceScope,
    ) -> Result<(), Error> {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .authorize_peer(peer, credentials, scope)
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
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        if peer == inner.config.node.public_key() {
            return Err(denied("a replica cannot acknowledge itself"));
        }
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
            if !holds_whole(&grants, frontier.scope) || !inner.holds(frontier.scope).await? {
                replies.push(FrontierReply::Refused(frontier.scope));
                continue;
            }
            replies.push(FrontierReply::Held(FrontierState {
                frontier: inner.frontier(frontier.scope).await?,
                importing: inner.importing(frontier.scope).await?,
            }));
        }
        Ok(replies)
    }
    pub(crate) async fn held_of(
        &self,
        scopes: &[ResourceScope],
    ) -> Result<Vec<ResourceScope>, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        let mut held = Vec::new();
        for scope in scopes {
            if inner.holds(*scope).await? {
                held.push(*scope);
            }
        }
        Ok(held)
    }
    pub(crate) async fn peer_holds(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scopes: &[ResourceScope],
    ) -> Result<Vec<ResourceScope>, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        if inner.peer_tip(peer) != Some(inner.workspace.tip) {
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

fn base_covers(request: &ScopeFrontier, base: &[super::checkpoint::LogPosition]) -> bool {
    request.logs.iter().all(|log| {
        base.iter().any(|covered| {
            covered.author == log.author && log.held <= covered.sequence.saturating_add(1)
        })
    })
}

#[cfg(test)]
mod frontier_tests {
    use super::*;
    use crate::replica::checkpoint::LogPosition;

    fn log(author: PublicKey, held: u64, applied: u64) -> FrontierLog {
        FrontierLog {
            author,
            held,
            applied,
        }
    }
    fn frontier(logs: Vec<FrontierLog>) -> ScopeFrontier {
        let scope = rocky_core::Scope {
            mesh: rocky_core::MeshId([1; 16]),
            app: rocky_core::AppId([2; 16]),
            domain: rocky_core::DomainId([3; 16]),
        };
        ScopeFrontier {
            scope: ResourceScope {
                domain: scope,
                encryption: [4; 32],
                table: [5; 32],
            },
            logs,
        }
    }
    fn position(author: PublicKey, sequence: u64) -> LogPosition {
        LogPosition {
            author,
            sequence,
            hash: Hash::from_bytes([6; 32]),
            clock: 1,
        }
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn a_base_must_cover_every_named_log_even_one_with_nothing_held() {
        let (a, b) = (
            PrivateKey::new().public_key(),
            PrivateKey::new().public_key(),
        );
        let request = frontier(vec![log(a, 4, 4), log(b, 0, 0)]);
        assert!(!base_covers(&request, &[position(a, 9)]));
        assert!(base_covers(&request, &[position(a, 3), position(b, 0)]));
        assert!(!base_covers(&request, &[position(a, 2), position(b, 0)]));
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn lag_counts_only_the_applied_operations_the_other_frontier_is_ahead_by() {
        let (a, b) = (
            PrivateKey::new().public_key(),
            PrivateKey::new().public_key(),
        );
        let own = frontier(vec![log(a, 9, 5)]);
        let other = frontier(vec![log(a, 9, 3), log(b, 7, 2)]);
        assert_eq!(own.lag_behind(&other), 2);
        assert_eq!(other.lag_behind(&own), 2);
    }
}
