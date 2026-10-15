//! Durable, domain-scoped tip graph and the selected live workspace.
use super::*;
use rocky_core::Scope;
use rocky_credentials::{SignedBookmarkMove, SignedSeal, SignedTipOpening, TipGraph};

pub(super) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_tip_graphs(scope BLOB PRIMARY KEY,genesis BLOB NOT NULL) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_tip_statements(digest BLOB PRIMARY KEY,statement BLOB NOT NULL) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_followed_tip(id INTEGER PRIMARY KEY CHECK(id=1),scope BLOB NOT NULL,bookmark TEXT NOT NULL,tip BLOB NOT NULL)",
    "CREATE TABLE IF NOT EXISTS secure_node_tips(scope BLOB NOT NULL,node BLOB NOT NULL,tip BLOB NOT NULL,PRIMARY KEY(scope,node)) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_line_nodes(line BLOB PRIMARY KEY,node BLOB NOT NULL UNIQUE) WITHOUT ROWID",
];

/// A graph record is always exchanged independently of row subscriptions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TipStatement {
    Seal(SignedSeal),
    Opening(SignedTipOpening),
    Bookmark(SignedBookmarkMove),
}
impl TipStatement {
    pub fn id(&self) -> Result<Commitment, Error> {
        match self {
            Self::Seal(s) => s.id(),
            Self::Opening(s) => s.id(),
            Self::Bookmark(s) => s.id(),
        }
        .map_err(credential)
    }
    pub fn scope(&self) -> Scope {
        match self {
            Self::Seal(s) => s.statement.scope,
            Self::Opening(s) => s.statement.scope,
            Self::Bookmark(s) => s.statement.scope,
        }
    }
    fn append(&self, graph: &mut TipGraph) {
        match self {
            Self::Seal(s) => graph.seals.push(s.clone()),
            Self::Opening(s) => graph.openings.push(s.clone()),
            Self::Bookmark(s) => graph.moves.push(s.clone()),
        }
    }
    fn verify(
        &self,
        publisher: PublicKey,
        graph: &TipGraph,
        current: Option<(&RevocationSet, u64)>,
    ) -> Result<(), Error> {
        match (self, current) {
            (Self::Seal(s), Some((r, t))) => s.verify_current(publisher, graph, r, t),
            (Self::Opening(s), Some((r, t))) => s.verify_current(publisher, graph, r, t),
            (Self::Bookmark(s), Some((r, t))) => s.verify_current(publisher, graph, r, t),
            (Self::Seal(s), None) => s.verify(publisher, graph),
            (Self::Opening(s), None) => s.verify(publisher, graph),
            (Self::Bookmark(s), None) => s.verify(publisher, graph),
        }
        .map_err(credential)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FrameReach {
    Live,
    Sealed,
    Held,
    Unfollowed,
}

/// An enrolled node for one line; its key belongs in the caller's secret store.
#[derive(Clone)]
pub struct LineNode {
    pub key: PrivateKey,
    pub credentials: CredentialChain,
}

impl<A: RowApplication> Inner<A> {
    pub(crate) fn graph_for(&self, scope: Scope) -> Result<&TipGraph, Error> {
        self.graphs
            .get(&scope)
            .ok_or_else(|| denied("unknown tip graph"))
    }
    pub(crate) fn manifest_of_tip(&self, tip: TipId) -> Result<Commitment, Error> {
        for graph in self.graphs.values() {
            if let Ok(schema) = graph.schema_any(tip) {
                return Ok(schema);
            }
        }
        std::iter::once(self.manifest)
            .chain(self.previous.iter().map(|previous| previous.manifest))
            .find(|manifest| rocky_credentials::genesis_tip(*manifest) == tip)
            .ok_or_else(|| Error::Incompatible("unknown tip".into()))
    }
    pub(crate) fn same_tip_peer(&self, peer: PublicKey) -> Result<(), Error> {
        if self.peer_tip(peer) != Some(self.tip) {
            return Err(denied("peer follows another tip"));
        }
        Ok(())
    }
    fn scope_graph(&self, scope: Scope) -> Result<TipGraph, Error> {
        if scope.app != self.definition.app || !self.domains.contains(&scope.domain) {
            return Err(denied("tip graph belongs to another domain"));
        }
        Ok(self
            .graphs
            .get(&scope)
            .cloned()
            .unwrap_or_else(|| TipGraph::new(scope, self.manifest)))
    }
    pub(crate) fn frame_reach(
        &self,
        scope: Scope,
        tip: TipId,
        author: u32,
        sequence: u64,
    ) -> Result<FrameReach, Error> {
        let Some(graph) = self.graphs.get(&scope) else {
            return Ok(if tip == self.tip {
                FrameReach::Live
            } else {
                FrameReach::Unfollowed
            });
        };
        if graph.schema(tip).is_err() && graph.schema_any(tip).is_ok() {
            return Ok(FrameReach::Held);
        }
        if !self.visible_frame(graph, tip, author, sequence, self.tip)?
            && self.abandoned_frame(scope, graph, tip, author, sequence)?
        {
            return Ok(FrameReach::Held);
        }
        // The active tip can be sealed while its bookmark waits for a move.
        if tip == self.tip {
            return Ok(match graph.seal(tip).map_err(credential)? {
                None => FrameReach::Live,
                Some(seal)
                    if seal
                        .statement
                        .frontier
                        .iter()
                        .any(|m| m.author == author && sequence <= m.sequence) =>
                {
                    FrameReach::Sealed
                }
                Some(seal)
                    if matches!(seal.statement.action, rocky_credentials::SealAction::Close) =>
                {
                    FrameReach::Held
                }
                Some(_) => FrameReach::Unfollowed,
            });
        }
        let mut cursor = tip;
        let mut visited = BTreeSet::new();
        while visited.insert(cursor) {
            let Some(seal) = graph.seal(cursor).map_err(credential)? else {
                return Ok(if cursor == self.tip {
                    FrameReach::Live
                } else {
                    FrameReach::Unfollowed
                });
            };
            let included = seal
                .statement
                .frontier
                .iter()
                .any(|m| m.author == author && sequence <= m.sequence);
            if included && self.visible_frame(graph, tip, author, sequence, self.tip)? {
                return Ok(FrameReach::Sealed);
            }
            match seal.successor().map_err(credential)? {
                Some(next) => cursor = next,
                None => return Ok(FrameReach::Held),
            }
        }
        Ok(FrameReach::Unfollowed)
    }
    fn abandoned_frame(
        &self,
        scope: Scope,
        graph: &TipGraph,
        source: TipId,
        author: u32,
        sequence: u64,
    ) -> Result<bool, Error> {
        let Ok(source_line) = graph.line_root(source) else {
            return Ok(false);
        };
        let names = graph
            .moves
            .iter()
            .map(|movement| &movement.statement.name)
            .collect::<BTreeSet<_>>();
        for name in names {
            let mut cursor = None;
            let mut visited = BTreeSet::new();
            while visited.insert(cursor) {
                let mut winner = None;
                for movement in graph.moves.iter().filter(|movement| {
                    &movement.statement.name == name && movement.statement.from == cursor
                }) {
                    let target = match movement
                        .statement
                        .to
                        .map(|tip| graph.canonical_tip(tip))
                        .transpose()
                    {
                        Ok(target) => target,
                        Err(_) => continue,
                    };
                    let digest = movement.id().map_err(credential)?;
                    if winner.as_ref().is_none_or(|(known, _)| digest < *known) {
                        winner = Some((digest, target));
                    }
                }
                let Some((_, target)) = winner else { break };
                if let (Some(from), Some(to)) = (cursor, target)
                    && !graph.continues_any(from, to).map_err(credential)?
                    && graph.line_root(from).is_ok_and(|line| line == source_line)
                {
                    let common = self.common_frontier(scope, from, to)?;
                    if !common
                        .iter()
                        .any(|mark| mark.author == author && sequence <= mark.sequence)
                    {
                        return Ok(true);
                    }
                }
                cursor = target;
            }
        }
        Ok(false)
    }
    /// A branch inherits only its opening seal's frontier. Continuations take late writes.
    pub(super) fn visible_frame(
        &self,
        graph: &TipGraph,
        source: TipId,
        author: u32,
        sequence: u64,
        target: TipId,
    ) -> Result<bool, Error> {
        if let Some(seal) = graph.seal(target).map_err(credential)?
            && !seal
                .statement
                .frontier
                .iter()
                .any(|mark| mark.author == author && sequence <= mark.sequence)
        {
            return Ok(false);
        }
        let mut cursor = target;
        let mut visited = BTreeSet::new();

        while visited.insert(cursor) {
            if cursor == source {
                return Ok(true);
            }
            let mut parent = None;
            for seal in &graph.seals {
                if seal.successor().map_err(credential)? == Some(cursor) {
                    // Only the canonical continuation supplies live state.
                    if graph
                        .seal(seal.statement.tip)
                        .map_err(credential)?
                        .is_none_or(|winner| winner != seal)
                    {
                        return Ok(false);
                    }

                    parent = Some(seal.statement.tip);
                    break;
                }
            }
            if let Some(tip) = parent {
                cursor = tip;
                continue;
            }
            for opening in &graph.openings {
                if opening.tip().map_err(credential)? == cursor {
                    let seal = graph
                        .seal_by_id(opening.statement.seal)
                        .map_err(credential)?;
                    if !seal
                        .statement
                        .frontier
                        .iter()
                        .any(|m| m.author == author && sequence <= m.sequence)
                    {
                        return Ok(false);
                    }
                    parent = Some(seal.statement.tip);
                    break;
                }
            }
            match parent {
                Some(tip) => cursor = tip,
                None => return Ok(false),
            }
        }
        Err(denied("cyclic tip graph"))
    }
    pub(super) fn ensure_tip_writable(&self, scope: Scope) -> Result<(), Error> {
        if let Some((followed, name)) = &self.followed
            && *followed == scope
            && self.graph_for(scope)?.bookmark(name).map_err(credential)? != Some(self.tip)
        {
            return Err(denied(
                "followed bookmark moved or was deleted; workspace is read-only",
            ));
        }
        if let Some(graph) = self.graphs.get(&scope)
            && graph.seal(self.tip).map_err(credential)?.is_some()
        {
            return Err(denied("followed bookmark points to a sealed tip"));
        }
        if self.node_author.is_none() {
            return Err(denied("this line needs an enrolled node of the identity"));
        }
        Ok(())
    }
    pub(super) async fn tip_frontier(&self, scope: ResourceScope) -> Result<ScopeFrontier, Error> {
        let mut positions = BTreeMap::<PublicKey, BTreeMap<u64, bool>>::new();
        let records=collect_rows(self.conn.query("SELECT header,status FROM secure_operations WHERE scope=?1 UNION ALL SELECT header,'applied' FROM secure_voids WHERE scope=?1",[encode(&scope)?]).await?).await?;
        for record in records {
            let [SqlValue::Blob(bytes), SqlValue::Text(status)] = record.as_slice() else {
                return Err(Error::Corrupt("frontier header".into()));
            };
            let header: p2panda_core::Header<Extensions> = decode(bytes)?;
            let e = &header.extensions;
            let visible = match self.graphs.get(&scope.domain) {
                Some(graph) => self.visible_frame(
                    graph,
                    e.tip,
                    e.author.ok_or_else(|| denied("node author missing"))?,
                    e.author_sequence,
                    self.tip,
                )?,
                None => e.tip == self.tip,
            };
            if visible && (status != "held" || self.graphs.contains_key(&scope.domain)) {
                positions.entry(header.public_key).or_default().insert(
                    header.seq_num,
                    matches!(status.as_str(), "applied" | "abandoned" | "held"),
                );
            }
        }
        let mut starts = BTreeMap::new();
        if let Some(id) = self.find_scope_id(scope).await? {
            for (log, horizon) in self.logs(id, None, false, MAX_CHECKPOINT_LOGS + 1).await? {
                if horizon >= 0 {
                    starts.insert(log.author, horizon as u64 + 1);
                }
            }
        }
        let mut logs = Vec::new();
        for author in positions
            .keys()
            .chain(starts.keys())
            .copied()
            .collect::<BTreeSet<_>>()
        {
            let mut held = starts.get(&author).copied().unwrap_or(0);
            let mut applied = held;
            if let Some(positions) = positions.get(&author) {
                while let Some(is_applied) = positions.get(&held) {
                    if *is_applied && applied == held {
                        applied += 1;
                    }
                    held += 1;
                }
            }
            if held > 0 {
                logs.push(FrontierLog {
                    author,
                    held,
                    applied,
                });
            }
        }
        if logs.len() > MAX_CHECKPOINT_LOGS {
            return Err(denied("logs per scope quota"));
        }
        Ok(ScopeFrontier {
            scope,
            cuts: self.held_cuts(self.tip_log(scope.domain)).len() as u64,
            logs,
        })
    }
    pub(super) async fn load_tip_graphs(&mut self) -> Result<(), Error> {
        let mut graphs = BTreeMap::new();
        for record in rows(&self.conn, "SELECT scope,genesis FROM secure_tip_graphs").await? {
            let [SqlValue::Blob(scope), SqlValue::Blob(genesis)] = record.as_slice() else {
                return Err(Error::Corrupt("tip graph".into()));
            };
            let scope: Scope = decode(scope)?;
            let schema: Commitment = genesis
                .as_slice()
                .try_into()
                .map_err(|_| Error::Corrupt("genesis schema".into()))?;
            self.known(schema)?;
            graphs.insert(scope, TipGraph::new(scope, schema));
        }
        let mut statements = Vec::new();
        for record in rows(
            &self.conn,
            "SELECT digest,statement FROM secure_tip_statements ORDER BY digest",
        )
        .await?
        {
            let [SqlValue::Blob(digest), SqlValue::Blob(bytes)] = record.as_slice() else {
                return Err(Error::Corrupt("tip statement".into()));
            };
            let statement: TipStatement =
                decode_bounded(bytes, rocky_credentials::MAX_CREDENTIAL_BYTES + 64)?;
            if statement.id()?.as_slice() != digest {
                return Err(Error::Corrupt("tip statement digest".into()));
            }
            statement.append(
                graphs
                    .get_mut(&statement.scope())
                    .ok_or_else(|| Error::Corrupt("tip statement graph".into()))?,
            );
            statements.push(statement);
        }
        for statement in statements {
            statement.verify(self.config.publisher, &graphs[&statement.scope()], None)?;
        }
        self.graphs = graphs;
        self.followed = None;
        self.tip = rocky_credentials::genesis_tip(self.manifest);
        if let Some(record) = rows(
            &self.conn,
            "SELECT scope,bookmark,tip FROM secure_followed_tip WHERE id=1",
        )
        .await?
        .first()
        {
            let [
                SqlValue::Blob(scope),
                SqlValue::Text(name),
                SqlValue::Blob(tip),
            ] = record.as_slice()
            else {
                return Err(Error::Corrupt("followed tip".into()));
            };
            let scope: Scope = decode(scope)?;
            let tip: TipId = decode(tip)?;
            self.graph_for(scope)?.schema(tip).map_err(credential)?;
            self.followed = Some((scope, name.clone()));
            self.tip = tip;
        }
        let genesis = rocky_credentials::genesis_tip(self.manifest);
        let original = identity::NodeAuthor::new(&self.config)?;
        self.line_nodes.entry(genesis).or_insert(original);
        self.conn
            .execute(
                "INSERT OR IGNORE INTO secure_line_nodes VALUES(?,?)",
                [
                    encode(&genesis)?,
                    self.config.node.public_key().as_bytes().to_vec(),
                ],
            )
            .await?;
        if let Some((scope, _)) = self.followed.clone() {
            self.select_line(scope, self.tip).await?;
        }
        Ok(())
    }
    async fn store_followed(&self, scope: Scope, name: &str, tip: TipId) -> Result<(), Error> {
        self.conn.execute("INSERT INTO secure_followed_tip VALUES(1,?,?,?) ON CONFLICT(id) DO UPDATE SET scope=excluded.scope,bookmark=excluded.bookmark,tip=excluded.tip",bindings(vec![SqlValue::Blob(encode(&scope)?),SqlValue::Text(name.into()),SqlValue::Blob(encode(&tip)?)] )).await?;
        self.conn
            .execute(
                "UPDATE secure_metadata SET identity=? WHERE id=1",
                [generation::identity(tip, self.config.node.public_key())?],
            )
            .await?;
        Ok(())
    }
    async fn select_line(&mut self, scope: Scope, target: TipId) -> Result<(), Error> {
        let root = self
            .graph_for(scope)?
            .line_root(target)
            .map_err(credential)?;
        let records = collect_rows(
            self.conn
                .query(
                    "SELECT node FROM secure_line_nodes WHERE line=?",
                    [encode(&root)?],
                )
                .await?,
        )
        .await?;
        let mut selected = match records.first().map(Vec::as_slice) {
            Some([SqlValue::Blob(node)]) => self
                .line_nodes
                .get(&root)
                .filter(|author| author.key.public_key().as_bytes().as_slice() == node)
                .cloned(),
            None if root
                == rocky_credentials::genesis_tip(self.graph_for(scope)?.genesis_schema) =>
            {
                Some(identity::NodeAuthor::new(&self.config)?)
            }
            _ => None,
        };
        if let Some(author) = &selected
            && !self
                .node_continues(scope, target, author.key.public_key())
                .await?
        {
            selected = None;
        }
        if let Some(author) = &selected {
            self.conn
                .execute(
                    "INSERT OR IGNORE INTO secure_line_nodes VALUES(?,?)",
                    [encode(&root)?, author.key.public_key().as_bytes().to_vec()],
                )
                .await?;
            self.line_nodes.insert(root, author.clone());
        }
        self.node_author = selected;
        Ok(())
    }
    async fn node_continues(
        &self,
        scope: Scope,
        target: TipId,
        node: PublicKey,
    ) -> Result<bool, Error> {
        let records = collect_rows(
            self.conn
                .query(
                    "SELECT tip FROM secure_node_tips WHERE scope=? AND node=?",
                    [encode(&scope)?, node.as_bytes().to_vec()],
                )
                .await?,
        )
        .await?;
        match records.first().map(Vec::as_slice) {
            None => Ok(true),
            Some([SqlValue::Blob(bytes)]) => {
                let previous: TipId = decode(bytes)?;
                Ok(previous == target
                    || self
                        .graph_for(scope)?
                        .continues(previous, target)
                        .unwrap_or(false))
            }
            _ => Err(Error::Corrupt("node tip".into())),
        }
    }

    async fn follow_target(
        &mut self,
        scope: Scope,
        name: &str,
        target: TipId,
    ) -> Result<(), Error> {
        let tip = self.tip;
        let author = self.node_author.clone();
        let nodes = self.line_nodes.clone();
        let result = transaction!(self, |inner| {
            inner.tip = tip;
            inner.node_author = author.clone();
            inner.line_nodes = nodes.clone();
            inner.switch_tip(scope, name, target).await
        });
        if let Err(error) = result {
            self.tip = tip;
            self.node_author = author;
            self.line_nodes = nodes;
            return Err(error);
        }
        self.followed = Some((scope, name.into()));
        self.publish_revision().await?;
        Ok(())
    }
    async fn switch_tip(&mut self, scope: Scope, name: &str, target: TipId) -> Result<(), Error> {
        let graph = self.graph_for(scope)?;
        let schema = graph.schema(target).map_err(credential)?;
        if schema != self.manifest {
            return Err(Error::Incompatible(
                "followed tip schema is not installed".into(),
            ));
        }
        let forward = target == self.tip
            || graph
                .continues(self.tip, target)
                .is_ok_and(|continues| continues);
        let old_tip = self.tip;
        let old_author = self.node_author.clone();
        let old_nodes = self.line_nodes.clone();
        let leaving_line = !forward
            && self
                .followed
                .as_ref()
                .is_some_and(|(followed, bookmark)| *followed == scope && bookmark == name);
        if leaving_line {
            let common = self.common_frontier(scope, self.tip, target)?;
            let mut tips = Vec::new();
            let mut cursor = self.tip;
            let mut visited = BTreeSet::new();
            while visited.insert(cursor) {
                tips.push(cursor);
                match Self::tip_parent(self.graph_for(scope)?, cursor)? {
                    Some((parent, _)) => cursor = parent,
                    None => break,
                }
            }
            for tip in tips {
                self.hold_line_writes(scope, tip, &common).await?;
            }
        }
        self.tip = target;
        let result = async {
            self.select_line(scope, target).await?;
            if !forward {
                self.rebuild_tip_workspace(scope).await?;
            }
            self.store_followed(scope, name, target).await
        }
        .await;
        if result.is_err() {
            self.tip = old_tip;
            self.node_author = old_author;
            self.line_nodes = old_nodes;
        }
        result
    }
    fn common_frontier(
        &self,
        scope: Scope,
        from: TipId,
        to: TipId,
    ) -> Result<Vec<rocky_credentials::AuthorMark>, Error> {
        let graph = self.graph_for(scope)?;
        let mut ancestors = BTreeSet::new();
        let mut cursor = from;
        while ancestors.insert(cursor) {
            match Self::tip_parent(graph, cursor)? {
                Some((parent, _)) => cursor = parent,
                None => break,
            }
        }
        let mut cursor = to;
        let mut visited = BTreeSet::new();
        while visited.insert(cursor) {
            if ancestors.contains(&cursor) {
                return Ok(graph
                    .seal(cursor)
                    .map_err(credential)?
                    .map(|seal| seal.statement.frontier.clone())
                    .unwrap_or_default());
            }
            match Self::tip_parent(graph, cursor)? {
                Some((parent, frontier)) => {
                    if ancestors.contains(&parent) {
                        return Ok(frontier);
                    }
                    cursor = parent;
                }
                None => break,
            }
        }
        Ok(Vec::new())
    }
    pub(super) fn tip_parent(
        graph: &TipGraph,
        tip: TipId,
    ) -> Result<Option<(TipId, Vec<rocky_credentials::AuthorMark>)>, Error> {
        for seal in &graph.seals {
            if seal.successor().map_err(credential)? == Some(tip) {
                return Ok(Some((seal.statement.tip, seal.statement.frontier.clone())));
            }
        }
        for opening in &graph.openings {
            if opening.tip().map_err(credential)? == tip {
                let seal = graph
                    .seal_by_id(opening.statement.seal)
                    .map_err(credential)?;
                return Ok(Some((seal.statement.tip, seal.statement.frontier.clone())));
            }
        }
        Ok(None)
    }
    async fn rebuild_tip_workspace(&mut self, domain: Scope) -> Result<(), Error> {
        self.index_inherited_held(domain).await?;
        let scopes = rows(&self.conn, "SELECT scope FROM secure_scope_state").await?;
        for record in scopes {
            let [SqlValue::Blob(scope)] = record.as_slice() else {
                return Err(Error::Corrupt("scope".into()));
            };
            let scope: ResourceScope = decode(scope)?;
            if scope.domain != domain {
                continue;
            }
            if self.has_horizon(scope).await? {
                return Err(denied(
                    "switching tips needs retained history or a target checkpoint",
                ));
            }
            self.rebuilding_tip = true;
            let rebuilt = self.rebuild_rows(scope).await;
            self.rebuilding_tip = false;
            rebuilt?;
            let id = self.scope_id(scope).await?;
            for table in [
                "secure_checkpoints",
                "secure_checkpoint_rows",
                "secure_checkpoint_overlay",
                "secure_checkpoint_scratch",
            ] {
                self.conn
                    .execute(format!("DELETE FROM {table} WHERE scope=?"), [id])
                    .await?;
            }
            self.rebuild_tip_existence(scope).await?;
            self.rebuild_constraints(Some(scope), None).await?;
        }
        Ok(())
    }
    async fn index_inherited_held(&self, domain: Scope) -> Result<(), Error> {
        let records = rows(&self.conn, "SELECT o.header,o.body FROM secure_operations o LEFT JOIN secure_applied a ON a.hash=o.hash WHERE o.status='held' AND a.hash IS NULL").await?;
        let mut inherited = BTreeMap::new();
        for record in records {
            let [SqlValue::Blob(header), SqlValue::Blob(body)] = record.as_slice() else {
                return Err(Error::Corrupt("held replay history".into()));
            };
            let frame = self.stored_frame(header.clone(), body.clone()).await?;
            let operation = frame.operation()?;
            if operation.header.extensions.scope.domain == domain
                && self.visible_held_in_workspace(&operation)?
            {
                inherited.insert(
                    (operation.header.timestamp, *operation.hash.as_bytes()),
                    (operation, frame),
                );
            }
        }
        // Held custody has no applied-log tip. Index only the selected sealed
        // history, in causal clock order, without advancing any author log.
        for (_, (operation, frame)) in inherited {
            if !self.dependencies_ready(&frame, &BTreeSet::new()).await? {
                return Err(denied("inherited held snapshot has missing dependencies"));
            }
            let scope = self.scope_id(operation.header.extensions.scope).await?;
            self.conn
                .execute(
                    "INSERT INTO secure_applied(scope,hash) VALUES(?,?)",
                    bindings(vec![
                        SqlValue::Integer(scope),
                        SqlValue::Blob(operation.hash.as_bytes().to_vec()),
                    ]),
                )
                .await?;
        }
        Ok(())
    }
    pub(super) fn visible_in_workspace(
        &self,
        operation: &p2panda_core::Operation<Extensions>,
    ) -> Result<bool, Error> {
        let e = &operation.header.extensions;
        match self.graphs.get(&e.scope.domain) {
            Some(graph) => self.visible_frame(
                graph,
                e.tip,
                e.author.ok_or_else(|| denied("node author missing"))?,
                e.author_sequence,
                self.tip,
            ),
            None => Ok(e.tip == self.tip),
        }
    }
    pub(super) fn visible_held_in_workspace(
        &self,
        operation: &p2panda_core::Operation<Extensions>,
    ) -> Result<bool, Error> {
        Ok(self
            .graphs
            .contains_key(&operation.header.extensions.scope.domain)
            && self.visible_in_workspace(operation)?)
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    pub async fn followed_tip(&self) -> TipId {
        self.inner.lock().await.tip
    }
    pub async fn followed_bookmark(&self) -> Option<(Scope, String)> {
        self.inner.lock().await.followed.clone()
    }
    pub async fn tip_graph(&self, scope: Scope) -> Result<TipGraph, Error> {
        self.inner.lock().await.scope_graph(scope)
    }
    /// Verified union; no row scan is needed for seals or forward bookmark moves.
    pub async fn receive_tip_statements(
        &self,
        statements: &[TipStatement],
    ) -> Result<usize, Error> {
        if self.domain_log().is_some()
            && statements
                .iter()
                .any(|s| matches!(s, TipStatement::Seal(_) | TipStatement::Bookmark(_)))
        {
            return Err(denied(
                "coordinated tip seal or move requires a terminal log entry",
            ));
        }
        let mut inner = self.inner.lock().await;
        let mut graphs = inner.graphs.clone();
        let mut new = Vec::new();
        for statement in statements {
            let scope = statement.scope();
            if let std::collections::btree_map::Entry::Vacant(entry) = graphs.entry(scope) {
                entry.insert(inner.scope_graph(scope)?);
            }
            let id = statement.id()?;
            let exists = collect_rows(
                inner
                    .conn
                    .query(
                        "SELECT 1 FROM secure_tip_statements WHERE digest=?",
                        [id.to_vec()],
                    )
                    .await?,
            )
            .await?;
            if !exists.is_empty() || new.iter().any(|(known, _)| *known == id) {
                continue;
            }
            statement.append(
                graphs
                    .get_mut(&scope)
                    .ok_or_else(|| denied("unknown graph"))?,
            );
            new.push((id, statement));
        }
        if new.is_empty() {
            return Ok(0);
        }
        for (_, statement) in &new {
            statement.verify(
                inner.config.publisher,
                &graphs[&statement.scope()],
                Some((&inner.config.revocations, now()?)),
            )?;
        }
        for (_, statement) in &new {
            if let TipStatement::Bookmark(movement) = statement
                && let (Some(from), Some(to)) = (movement.statement.from, movement.statement.to)
            {
                let graph = &graphs[&movement.statement.scope];
                if !graph.continues_any(from, to).map_err(credential)?
                    && graph.seal(from).map_err(credential)?.is_none()
                {
                    return Err(denied(
                        "backward or sideways move must include a close seal for the tip it leaves",
                    ));
                }
            }
        }
        let old_graphs = inner.graphs.clone();
        let old_tip = inner.tip;
        let old_author = inner.node_author.clone();
        let old_nodes = inner.line_nodes.clone();
        let old_followed = inner.followed.clone();
        let result = transaction!(inner, |inner| {
            inner.graphs = graphs.clone();
            inner.tip = old_tip;
            inner.node_author = old_author.clone();
            inner.line_nodes = old_nodes.clone();
            inner.followed = old_followed.clone();
            for (scope, graph) in &graphs {
                inner
                    .conn
                    .execute(
                        "INSERT OR IGNORE INTO secure_tip_graphs VALUES(?,?)",
                        [encode(scope)?, graph.genesis_schema.to_vec()],
                    )
                    .await?;
            }
            for (id, statement) in &new {
                inner
                    .conn
                    .execute(
                        "INSERT INTO secure_tip_statements VALUES(?,?)",
                        [id.to_vec(), encode(statement)?],
                    )
                    .await?;
            }
            if let Some((scope, name)) = old_followed.clone()
                && let Some(target) = inner
                    .graph_for(scope)?
                    .bookmark(&name)
                    .map_err(credential)?
                && target != old_tip
            {
                inner.switch_tip(scope, &name, target).await?;
            }
            Ok(())
        });
        if let Err(error) = result {
            inner.graphs = old_graphs;
            inner.tip = old_tip;
            inner.node_author = old_author;
            inner.line_nodes = old_nodes;
            inner.followed = old_followed;
            return Err(error);
        }
        inner.bump_authority();
        inner.publish_revision().await?;
        inner.flush()?;
        Ok(new.len())
    }
    pub async fn tip_statements(
        &self,
        after: Option<Commitment>,
        limit: usize,
    ) -> Result<Vec<TipStatement>, Error> {
        if limit == 0 || limit > 32 {
            return Err(denied("tip graph page quota"));
        }
        let mut inner = self.inner.lock().await;
        inner.flush()?;
        let records=collect_rows(inner.conn.query("SELECT statement FROM secure_tip_statements WHERE digest>? ORDER BY digest LIMIT ?",bindings(vec![SqlValue::Blob(after.map(|v|v.to_vec()).unwrap_or_default()),SqlValue::Integer(limit as i64)])).await?).await?;
        records
            .into_iter()
            .map(|record| match record.as_slice() {
                [SqlValue::Blob(bytes)] => {
                    decode_bounded(bytes, rocky_credentials::MAX_CREDENTIAL_BYTES + 64)
                }
                _ => Err(Error::Corrupt("tip statement".into())),
            })
            .collect()
    }
    pub async fn follow_bookmark(&self, scope: Scope, name: &str) -> Result<TipId, Error> {
        let mut inner = self.inner.lock().await;
        let target = inner
            .graph_for(scope)?
            .bookmark(name)
            .map_err(credential)?
            .ok_or_else(|| denied("bookmark does not exist"))?;
        inner.follow_target(scope, name, target).await?;
        inner.bump_authority();
        inner.flush()?;
        Ok(target)
    }
    /// Enrolls through the identity, outside any database transaction.
    pub async fn enroll_line_node(
        &self,
        identity: &dyn rocky_credentials::IdentityProvider,
    ) -> Result<LineNode, Error> {
        let mut chain = self.inner.lock().await.config.credentials.clone();
        if chain
            .0
            .last()
            .is_some_and(|leaf| leaf.capability.kind == rocky_core::PrincipalKind::Node)
        {
            chain.0.pop();
        }
        let range = chain
            .0
            .last()
            .ok_or_else(|| denied("identity credential missing"))?
            .capability
            .authors()
            .map_err(credential)?
            .ok_or_else(|| denied("identity author range missing"))?;
        let author = identity.reserve_author(range).map_err(credential)?;
        let key = PrivateKey::new();
        let credentials = chain
            .complete(identity, key.public_key(), author)
            .map_err(credential)?;
        Ok(LineNode { key, credentials })
    }
    pub async fn register_line_node(&self, node: LineNode) -> Result<(), Error> {
        let mut inner = self.inner.lock().await;
        let (scope, _) = inner
            .followed
            .as_ref()
            .ok_or_else(|| denied("follow a bookmark first"))?;
        if !inner
            .node_continues(*scope, inner.tip, node.key.public_key())
            .await?
        {
            return Err(denied(
                "a node author log cannot cross a fork; enroll another node",
            ));
        }
        let root = inner
            .graph_for(*scope)?
            .line_root(inner.tip)
            .map_err(credential)?;
        let owner = inner
            .config
            .credentials
            .principal()
            .ok_or_else(|| denied("identity credential missing"))?;
        if node
            .credentials
            .principal()
            .ok_or_else(|| denied("identity credential missing"))?
            != owner
        {
            return Err(denied("line node belongs to another identity"));
        }
        inner.verify_chain(&node.credentials, node.key.public_key(), now()?)?;
        let selected = identity::NodeAuthor {
            number: node.credentials.author_number().map_err(credential)?,
            chain: Chain::new(node.credentials.clone())?,
            credentials: node.credentials,
            key: node.key,
        };
        transaction!(inner, |inner| {
            inner.bind_node_author(&selected.credentials).await?;
            let existing = collect_rows(
                inner
                    .conn
                    .query(
                        "SELECT line FROM secure_line_nodes WHERE node=?",
                        [selected.key.public_key().as_bytes().to_vec()],
                    )
                    .await?,
            )
            .await?;
            let expected = vec![SqlValue::Blob(encode(&root)?)];
            if existing.iter().any(|row| row != &expected) {
                return Err(denied("a node author log belongs to one line"));
            }
            inner.conn.execute("INSERT INTO secure_line_nodes VALUES(?,?) ON CONFLICT(line) DO UPDATE SET node=excluded.node",[encode(&root)?,selected.key.public_key().as_bytes().to_vec()]).await?;
            Ok(())
        })?;
        inner.line_nodes.insert(root, selected.clone());
        inner.node_author = Some(selected);
        inner.flush()
    }
    /// Local seals/moves of a coordinated tip must enter its terminal log entry.
    pub async fn publish_tip_statements(
        &self,
        statements: &[TipStatement],
    ) -> Result<usize, Error> {
        if self.domain_log().is_some()
            && statements
                .iter()
                .any(|s| matches!(s, TipStatement::Seal(_) | TipStatement::Bookmark(_)))
        {
            return Err(denied(
                "coordinated tip seal or move requires a terminal log entry",
            ));
        }
        self.receive_tip_statements(statements).await
    }
}
