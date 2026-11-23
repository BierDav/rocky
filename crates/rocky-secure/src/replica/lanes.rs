use super::*;
use rocky_core::scope_label;
use rocky_credentials::PeerId;

pub(super) const SCHEMA: &[&str] = &[
    "CREATE TABLE secure_own_logs(scope BLOB NOT NULL,author BLOB NOT NULL,reported INTEGER,authored INTEGER NOT NULL,PRIMARY KEY(scope,author)) WITHOUT ROWID",
    "CREATE TABLE secure_retired_authors(author BLOB PRIMARY KEY,scope BLOB NOT NULL,held INTEGER NOT NULL,reported INTEGER NOT NULL) WITHOUT ROWID",
    "CREATE TABLE secure_peer_regressions(peer BLOB NOT NULL,scope BLOB NOT NULL,held INTEGER NOT NULL,reported INTEGER NOT NULL,PRIMARY KEY(peer,scope)) WITHOUT ROWID",
    "CREATE TABLE secure_unconfirmed(id INTEGER PRIMARY KEY CHECK(id=1))",
    "CREATE TABLE secure_confirmed_scopes(scope BLOB PRIMARY KEY) WITHOUT ROWID",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreSuperseded {
    pub node: PublicKey,
    pub scope: ResourceScope,
    pub held: u64,
    pub reported: u64,
}
impl std::fmt::Display for StoreSuperseded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "this device's store is older than what peers hold for its key ({} of {} positions); re-invite it as a new device",
            self.held, self.reported
        )
    }
}
impl std::error::Error for StoreSuperseded {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerRegression {
    pub node: PublicKey,
    pub scope: ResourceScope,
    pub held: u64,
    pub reported: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LaneAlerts {
    pub superseded: Vec<StoreSuperseded>,
    pub regressions: Vec<PeerRegression>,
}

#[derive(Default)]
pub(super) struct Lanes {
    settled: BTreeSet<(ResourceScope, PublicKey)>,
    taken: BTreeSet<(ResourceScope, PublicKey)>,
    ready: BTreeSet<PublicKey>,
}

impl Lanes {
    pub(super) fn forget_derived(&mut self) {
        self.settled.clear();
        self.taken.clear();
        self.ready.clear();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lane {
    Ready,
    Behind { held: u64, reported: u64 },
    Superseded { held: u64, reported: u64 },
}

struct OwnLog {
    reported: Option<u64>,
    authored: bool,
}

fn lane_blocker(scope: &ResourceScope, kind: BlockerKind, clears_by: ClearsBy) -> Blocker {
    Blocker {
        scope: scope_label(scope),
        kind,
        clears_by,
        since: None,
    }
}
fn count(value: i64) -> Result<u64, Error> {
    u64::try_from(value).map_err(|_| Error::Corrupt("negative log position".into()))
}
fn position(value: u64) -> SqlValue {
    SqlValue::Integer(value.min(i64::MAX as u64) as i64)
}

impl<A: RowApplication> Inner<A> {
    fn lanes(&self) -> std::sync::MutexGuard<'_, Lanes> {
        self.lanes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    fn own_authors(&self) -> BTreeSet<PublicKey> {
        let mut authors: BTreeSet<_> = self
            .line_nodes
            .values()
            .chain(&self.node_author)
            .map(|author| author.key.public_key())
            .collect();
        authors.insert(self.config.node.public_key());
        authors
    }
    pub(super) async fn mark_unconfirmed(&self) -> Result<(), Error> {
        self.lanes().ready.clear();
        self.conn
            .execute("INSERT OR IGNORE INTO secure_unconfirmed VALUES(1)", ())
            .await?;
        self.conn
            .execute("DELETE FROM secure_confirmed_scopes", ())
            .await?;
        Ok(())
    }
    async fn unconfirmed(&self) -> Result<bool, Error> {
        Ok(!rows(&self.conn, "SELECT 1 FROM secure_unconfirmed")
            .await?
            .is_empty())
    }
    async fn shared_scopes(&self) -> Result<BTreeSet<ResourceScope>, Error> {
        let mut shared = BTreeSet::new();
        for record in rows(
            &self.conn,
            "SELECT DISTINCT scope FROM secure_own_logs WHERE reported IS NOT NULL",
        )
        .await?
        {
            let [SqlValue::Blob(scope)] = record.as_slice() else {
                return Err(Error::Corrupt("own log scope".into()));
            };
            shared.insert(decode(scope)?);
        }
        Ok(shared)
    }
    async fn confirmed_scopes(&self) -> Result<BTreeSet<ResourceScope>, Error> {
        let mut confirmed = BTreeSet::new();
        for record in rows(&self.conn, "SELECT scope FROM secure_confirmed_scopes").await? {
            let [SqlValue::Blob(scope)] = record.as_slice() else {
                return Err(Error::Corrupt("confirmed scope".into()));
            };
            confirmed.insert(decode(scope)?);
        }
        Ok(confirmed)
    }
    async fn awaiting_frontiers(&self) -> Result<BTreeSet<ResourceScope>, Error> {
        if !self.unconfirmed().await? {
            return Ok(BTreeSet::new());
        }
        let mut shared = self.shared_scopes().await?;
        if shared.is_empty() {
            shared.extend(self.config.keys.iter().map(|key| key.scope));
        }
        let confirmed = self.confirmed_scopes().await?;
        let mut awaiting = BTreeSet::new();
        for scope in shared {
            if !confirmed.contains(&scope) && self.holds(scope).await? {
                awaiting.insert(scope);
            }
        }
        Ok(awaiting)
    }
    pub(super) async fn confirm_scopes(&self, scopes: &[ResourceScope]) -> Result<(), Error> {
        if !self.unconfirmed().await? {
            return Ok(());
        }
        let first_contact = self.shared_scopes().await?.is_empty();
        for scope in scopes.iter().filter(|_| !first_contact) {
            self.conn
                .execute(
                    "INSERT OR IGNORE INTO secure_confirmed_scopes VALUES(?)",
                    [encode(scope)?],
                )
                .await?;
        }
        if first_contact || self.awaiting_frontiers().await?.is_empty() {
            self.conn
                .execute("DELETE FROM secure_unconfirmed", ())
                .await?;
            self.conn
                .execute("DELETE FROM secure_confirmed_scopes", ())
                .await?;
        }
        Ok(())
    }
    async fn own_log(
        &self,
        scope: ResourceScope,
        author: PublicKey,
    ) -> Result<Option<OwnLog>, Error> {
        let records = collect_rows(
            self.conn
                .query(
                    "SELECT reported,authored FROM secure_own_logs WHERE scope=? AND author=?",
                    [encode(&scope)?, author.as_bytes().to_vec()],
                )
                .await?,
        )
        .await?;
        match records.first().map(Vec::as_slice) {
            None => Ok(None),
            Some([reported, SqlValue::Integer(authored)]) => Ok(Some(OwnLog {
                reported: match reported {
                    SqlValue::Null => None,
                    SqlValue::Integer(reported) => Some(count(*reported)?),
                    _ => return Err(Error::Corrupt("own log report".into())),
                },
                authored: *authored != 0,
            })),
            Some(_) => Err(Error::Corrupt("own log".into())),
        }
    }
    pub(super) async fn reported_own_log(
        &self,
        scope: ResourceScope,
        author: PublicKey,
    ) -> Result<u64, Error> {
        Ok(self
            .own_log(scope, author)
            .await?
            .and_then(|log| log.reported)
            .unwrap_or(0))
    }
    async fn local_next(&self, scope: ResourceScope, author: PublicKey) -> Result<u64, Error> {
        let Some(id) = self.find_scope_id(scope).await? else {
            return Ok(0);
        };
        match self.author_tip(scope, id, author).await? {
            Some(log) => count(log.sequence)?
                .checked_add(1)
                .ok_or_else(|| Error::Corrupt("own log position".into())),
            None => Ok(0),
        }
    }
    async fn lane(&self, scope: ResourceScope, author: PublicKey) -> Result<Lane, Error> {
        let Some(log) = self.own_log(scope, author).await? else {
            return Ok(Lane::Ready);
        };
        let reported = log.reported.unwrap_or(0);
        if reported == 0 {
            return Ok(Lane::Ready);
        }
        let held = self.local_next(scope, author).await?;
        Ok(if held >= reported {
            Lane::Ready
        } else if log.authored && !self.restoring(scope).await? {
            Lane::Superseded { held, reported }
        } else {
            Lane::Behind { held, reported }
        })
    }
    async fn unsettled_lanes(
        &self,
        author: PublicKey,
    ) -> Result<Vec<(ResourceScope, Lane)>, Error> {
        let mut unsettled = Vec::new();
        for record in collect_rows(
            self.conn
                .query(
                    "SELECT scope FROM secure_own_logs WHERE author=? AND reported>0 ORDER BY scope",
                    [author.as_bytes().to_vec()],
                )
                .await?,
        )
        .await?
        {
            let [SqlValue::Blob(scope)] = record.as_slice() else {
                return Err(Error::Corrupt("own log scope".into()));
            };
            let scope: ResourceScope = decode(scope)?;
            if self.lanes().settled.contains(&(scope, author)) || !self.holds(scope).await? {
                continue;
            }
            match self.lane(scope, author).await? {
                Lane::Ready => {
                    self.lanes().settled.insert((scope, author));
                }
                lane => unsettled.push((scope, lane)),
            }
        }
        Ok(unsettled)
    }
    async fn retired(&self, author: PublicKey) -> Result<Option<StoreSuperseded>, Error> {
        let records = collect_rows(
            self.conn
                .query(
                    "SELECT scope,held,reported FROM secure_retired_authors WHERE author=?",
                    [author.as_bytes().to_vec()],
                )
                .await?,
        )
        .await?;
        match records.first().map(Vec::as_slice) {
            None => Ok(None),
            Some(
                [
                    SqlValue::Blob(scope),
                    SqlValue::Integer(held),
                    SqlValue::Integer(reported),
                ],
            ) => Ok(Some(StoreSuperseded {
                node: author,
                scope: decode(scope)?,
                held: count(*held)?,
                reported: count(*reported)?,
            })),
            Some(_) => Err(Error::Corrupt("retired author".into())),
        }
    }
    pub(super) async fn superseded_fork(
        &self,
        header: &p2panda_core::Header<Extensions>,
    ) -> Result<bool, Error> {
        if !self.own_authors().contains(&header.public_key)
            || self.retired(header.public_key).await?.is_none()
        {
            return Ok(false);
        }
        Ok(!collect_rows(
            self.conn
                .query(
                    "SELECT 1 FROM secure_operations WHERE author=? AND scope=? AND sequence=? AND hash<>?",
                    bindings(vec![
                        SqlValue::Blob(header.public_key.as_bytes().to_vec()),
                        SqlValue::Blob(encode(&header.extensions.scope)?),
                        position(header.seq_num),
                        SqlValue::Blob(header.hash().as_bytes().to_vec()),
                    ]),
                )
                .await?,
        )
        .await?
        .is_empty())
    }
    async fn retire(
        &self,
        author: PublicKey,
        scope: ResourceScope,
        held: u64,
        reported: u64,
    ) -> Result<(), Error> {
        self.conn
            .execute(
                "INSERT OR IGNORE INTO secure_retired_authors VALUES(?,?,?,?)",
                bindings(vec![
                    SqlValue::Blob(author.as_bytes().to_vec()),
                    SqlValue::Blob(encode(&scope)?),
                    position(held),
                    position(reported),
                ]),
            )
            .await?;
        Ok(())
    }
    pub(super) async fn admit_lane(
        &self,
        scope: ResourceScope,
        author: PublicKey,
    ) -> Result<(), Error> {
        if self.lanes().ready.contains(&author) {
            return self.take_lane(scope, author).await;
        }
        if let Some(superseded) = self.retired(author).await? {
            return Err(Error::Application(Box::new(superseded)));
        }
        let mut blockers = Vec::new();
        if self.awaiting_frontiers().await?.contains(&scope) {
            blockers.push(lane_blocker(
                &scope,
                BlockerKind::AwaitingPeerFrontier,
                ClearsBy::AnyPeer,
            ));
        }
        for (behind, lane) in self.unsettled_lanes(author).await? {
            match lane {
                Lane::Ready => {}
                Lane::Behind { held, reported } => blockers.push(lane_blocker(
                    &behind,
                    BlockerKind::OwnLogBehind { held, reported },
                    ClearsBy::AnyPeer,
                )),
                Lane::Superseded { held, reported } => {
                    return Err(Error::Application(Box::new(StoreSuperseded {
                        node: author,
                        scope: behind,
                        held,
                        reported,
                    })));
                }
            }
        }
        blockers::refuse(blockers)?;
        if !self.unconfirmed().await? {
            self.lanes().ready.insert(author);
        }
        self.take_lane(scope, author).await
    }
    async fn take_lane(&self, scope: ResourceScope, author: PublicKey) -> Result<(), Error> {
        if self.lanes().taken.contains(&(scope, author)) {
            return Ok(());
        }
        if self
            .own_log(scope, author)
            .await?
            .is_some_and(|log| log.authored)
        {
            self.lanes().taken.insert((scope, author));
            return Ok(());
        }
        self.conn
            .execute(
                "INSERT INTO secure_own_logs VALUES(?,?,NULL,1) ON CONFLICT(scope,author) DO UPDATE SET authored=1",
                [encode(&scope)?, author.as_bytes().to_vec()],
            )
            .await?;
        let writer = writer(&author);
        let sealed = self
            .graphs
            .values()
            .flat_map(|graph| &graph.seals)
            .flat_map(|seal| &seal.statement.frontier)
            .filter(|mark| mark.author == writer)
            .map(|mark| mark.sequence)
            .max();
        match sealed {
            Some(sequence) => self.raise_author_sequence(author, sequence).await,
            None => Ok(()),
        }
    }
    pub(super) async fn raise_author_sequence(
        &self,
        author: PublicKey,
        used: u64,
    ) -> Result<(), Error> {
        self.conn
            .execute(
                "INSERT INTO secure_author_sequences VALUES(?,?) ON CONFLICT(node) DO UPDATE SET sequence=MAX(sequence,excluded.sequence)",
                bindings(vec![
                    SqlValue::Blob(author.as_bytes().to_vec()),
                    position(used),
                ]),
            )
            .await?;
        Ok(())
    }
    pub(super) async fn record_own_reports(&self, frontier: &ScopeFrontier) -> Result<(), Error> {
        let scope = frontier.scope;
        for author in self.own_authors() {
            let reported = frontier.log(author).map_or(0, |log| log.held);
            self.conn
                .execute(
                    "INSERT INTO secure_own_logs VALUES(?,?,?,0) ON CONFLICT(scope,author) DO UPDATE SET reported=MAX(COALESCE(reported,0),excluded.reported)",
                    bindings(vec![
                        SqlValue::Blob(encode(&scope)?),
                        SqlValue::Blob(author.as_bytes().to_vec()),
                        position(reported),
                    ]),
                )
                .await?;
            if reported == 0 {
                continue;
            }
            {
                let mut lanes = self.lanes();
                lanes.settled.remove(&(scope, author));
                lanes.ready.remove(&author);
            }
            if let Lane::Superseded { held, reported } = self.lane(scope, author).await? {
                self.retire(author, scope, held, reported).await?;
            }
        }
        Ok(())
    }
    pub(super) async fn record_lane_reports(
        &mut self,
        grants: &[rocky_core::ResourceGrant],
        frontiers: &[ScopeFrontier],
    ) -> Result<(), Error> {
        let reported: Vec<_> = frontiers
            .iter()
            .filter(|frontier| {
                [Right::Read, Right::Custody].iter().any(|right| {
                    grants.iter().any(|grant| {
                        grant.scope == frontier.scope
                            && grant.rows == RowSelection::All
                            && grant.rights.contains(right)
                    })
                })
            })
            .collect();
        transaction!(@keeping_frontier_cache self, |inner| {
            for frontier in &reported {
                inner.record_own_reports(frontier).await?;
            }
            let scopes: Vec<_> = reported.iter().map(|frontier| frontier.scope).collect();
            inner.confirm_scopes(&scopes).await
        })
    }
    pub(super) async fn forget_regressed_peer(
        &self,
        peer: PublicKey,
        frontier: &ScopeFrontier,
    ) -> Result<bool, Error> {
        let node = PeerId::of(peer).as_bytes().to_vec();
        let scope = encode(&frontier.scope)?;
        let reported = frontier.log(peer).map_or(0, |log| log.held);
        let stored = match collect_rows(
            self.conn
                .query(
                    "SELECT held FROM secure_peer_frontiers WHERE peer=? AND scope=? AND author=?",
                    [node.clone(), scope.clone(), peer.as_bytes().to_vec()],
                )
                .await?,
        )
        .await?
        .first()
        .map(Vec::as_slice)
        {
            None => 0,
            Some([SqlValue::Integer(held)]) => count(*held)?,
            Some(_) => return Err(Error::Corrupt("peer frontier".into())),
        };
        if reported >= stored {
            self.conn
                .execute(
                    "DELETE FROM secure_peer_regressions WHERE peer=? AND scope=? AND held<=?",
                    bindings(vec![
                        SqlValue::Blob(peer.as_bytes().to_vec()),
                        SqlValue::Blob(scope),
                        position(reported),
                    ]),
                )
                .await?;
            return Ok(false);
        }
        self.conn
            .execute(
                "DELETE FROM secure_peer_frontiers WHERE peer=? AND scope=?",
                [node, scope.clone()],
            )
            .await?;
        self.conn
            .execute(
                "INSERT INTO secure_peer_regressions VALUES(?,?,?,?) ON CONFLICT(peer,scope) DO UPDATE SET held=MAX(held,excluded.held),reported=excluded.reported",
                bindings(vec![
                    SqlValue::Blob(peer.as_bytes().to_vec()),
                    SqlValue::Blob(scope),
                    position(stored),
                    position(reported),
                ]),
            )
            .await?;
        Ok(true)
    }
    pub(super) async fn retract_lost_acknowledgments(
        &self,
        peer: PublicKey,
        scope: ResourceScope,
    ) -> Result<(), Error> {
        self.conn
            .execute(
                "DELETE FROM secure_ack WHERE peer=?1 AND hash IN (SELECT o.hash FROM secure_operations o WHERE o.scope=?2 AND o.sequence>=COALESCE((SELECT f.held FROM secure_peer_frontiers f WHERE f.peer=?1 AND f.scope=?2 AND f.author=o.author),0))",
                [PeerId::of(peer).as_bytes().to_vec(), encode(&scope)?],
            )
            .await?;
        Ok(())
    }
    pub(super) async fn lane_blockers(&self) -> Result<Vec<Blocker>, Error> {
        let mut blockers = Vec::new();
        let mut authors = BTreeSet::from([self.config.node.public_key()]);
        authors.extend(
            self.node_author
                .iter()
                .map(|author| author.key.public_key()),
        );
        for author in authors {
            if let Some(retired) = self.retired(author).await? {
                blockers.push(lane_blocker(
                    &retired.scope,
                    BlockerKind::StoreSuperseded {
                        held: retired.held,
                        reported: retired.reported,
                    },
                    ClearsBy::Application,
                ));
                continue;
            }
            for (scope, lane) in self.unsettled_lanes(author).await? {
                let (kind, clears_by) = match lane {
                    Lane::Ready => continue,
                    Lane::Behind { held, reported } => (
                        BlockerKind::OwnLogBehind { held, reported },
                        ClearsBy::AnyPeer,
                    ),
                    Lane::Superseded { held, reported } => (
                        BlockerKind::StoreSuperseded { held, reported },
                        ClearsBy::Application,
                    ),
                };
                blockers.push(lane_blocker(&scope, kind, clears_by));
            }
        }
        let shared = self.shared_scopes().await?;
        for scope in self
            .awaiting_frontiers()
            .await?
            .into_iter()
            .filter(|scope| shared.contains(scope))
        {
            blockers.push(lane_blocker(
                &scope,
                BlockerKind::AwaitingPeerFrontier,
                ClearsBy::AnyPeer,
            ));
        }
        Ok(blockers)
    }
    async fn lane_alerts(&self) -> Result<LaneAlerts, Error> {
        let mut alerts = LaneAlerts::default();
        for author in self.own_authors() {
            alerts.superseded.extend(self.retired(author).await?);
        }
        for record in rows(
            &self.conn,
            "SELECT peer,scope,held,reported FROM secure_peer_regressions ORDER BY peer,scope",
        )
        .await?
        {
            let [
                SqlValue::Blob(peer),
                SqlValue::Blob(scope),
                SqlValue::Integer(held),
                SqlValue::Integer(reported),
            ] = record.as_slice()
            else {
                return Err(Error::Corrupt("peer regression".into()));
            };
            alerts.regressions.push(PeerRegression {
                node: checkpoint::key_of(peer)?,
                scope: decode(scope)?,
                held: count(*held)?,
                reported: count(*reported)?,
            });
        }
        Ok(alerts)
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    pub(crate) async fn peer_holds_none_of(&self, scopes: &[ResourceScope]) -> Result<(), Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        transaction!(@keeping_frontier_cache inner, |inner| {
            inner.confirm_scopes(scopes).await
        })
    }
    pub async fn lane_alerts(&self) -> Result<LaneAlerts, Error> {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .lane_alerts()
            .await
    }
}
