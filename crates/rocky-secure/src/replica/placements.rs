//! Signed header placements: where a seal's frontier positions of scopes
//! this replica cannot hold lie, proved by each frame's author-signed header
//! without its body. See `docs/runtime/lazy-compaction.md`.
use super::*;
use rocky_core::Scope;

pub(super) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_placements(domain BLOB NOT NULL,author INTEGER NOT NULL,sequence INTEGER NOT NULL,scope BLOB NOT NULL,hash BLOB NOT NULL,header BLOB NOT NULL,PRIMARY KEY(domain,author,sequence)) WITHOUT ROWID",
    // Two signed headers of one node at one position: kept as evidence.
    "CREATE TABLE IF NOT EXISTS secure_equivocations(domain BLOB NOT NULL,author INTEGER NOT NULL,sequence INTEGER NOT NULL,first BLOB NOT NULL,second BLOB NOT NULL,PRIMARY KEY(domain,author,sequence)) WITHOUT ROWID",
];

/// Headers a placement page carries at most.
pub const MAX_PLACEMENTS: usize = 64;
/// Placement requests one pass sends at most.
const MAX_REQUESTS: usize = 16;

/// Positions `from..=to` of node `author` through `seal`'s frontier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlacementRequest {
    pub seal: Commitment,
    pub author: u32,
    pub from: u64,
    pub to: u64,
}
/// One frame's signed header, without its body.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Placement(#[serde(with = "serde_bytes")] pub Vec<u8>);
/// A placement answer.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum PlacementPage {
    /// The headers and, once each, the credential chains they name.
    Headers {
        headers: Vec<Placement>,
        chains: Vec<CredentialChain>,
    },
    /// A position lies in a partition the requester did not prove: its
    /// tuple is not shown, and only that partition's custody can settle it.
    CustodyRequired,
}

impl<A: RowApplication> Inner<A> {
    /// What this replica asks peers to place: the gaps that keep a seal of
    /// the followed path from being finalized, and those of the seal the
    /// followed line inherits.
    async fn wanted_placements(&self, domain: Scope) -> Result<Vec<PlacementRequest>, Error> {
        let path = self.path_seals(domain)?;
        let scopes = self.readiness_scopes(domain).await?;
        let held_scopes = self.custody_scopes(now()?)?;
        let (held, last, _) = self
            .held_positions(domain, &scopes, &held_scopes, &BTreeSet::new())
            .await?;
        let mut checks = Vec::new();
        for (index, seal) in path.iter().enumerate() {
            let id = seal.id().map_err(credential)?;
            if self.finalized_depth(domain, id).await?.is_some() {
                continue;
            }
            checks.push((seal, index.checked_sub(1).map(|previous| &path[previous])));
        }
        if !self.unverified_scopes(domain).await?.is_empty()
            && let Some(seal) = path.last()
        {
            checks.push((seal, None));
        }
        // A line's first coordinated slot checks its parent seal from the start.
        let graph = self.graph_for(domain)?;
        if let Some((parent, _)) = Self::tip_parent(graph, self.workspace.tip)?
            && let Some(seal) = path.iter().find(|seal| seal.statement.tip == parent)
        {
            checks.push((seal, None));
        }
        let mut requests: Vec<PlacementRequest> = Vec::new();
        for (seal, previous) in checks {
            let id = seal.id().map_err(credential)?;
            let mut gaps = BTreeSet::new();
            let unverified = self.unverified_scopes(domain).await?;
            for (scope, scope_id) in &scopes {
                let previous = if unverified.iter().any(|(unknown, _)| unknown == scope) {
                    None
                } else {
                    previous
                };
                let starts = self.seal_starts(*scope_id, previous).await?;
                gaps.extend(compaction::blocking_gaps(
                    seal,
                    &starts,
                    &[(*scope, *scope_id)],
                    &held,
                    &last,
                    MAX_REQUESTS * MAX_PLACEMENTS,
                ));
            }
            for (author, position) in gaps {
                if requests.iter().any(|request| {
                    request.author == author && (request.from..=request.to).contains(&position)
                }) {
                    continue;
                }
                if let Some(request) = requests.last_mut()
                    && request.seal == id
                    && request.author == author
                    && request.to + 1 == position
                    && position - request.from < MAX_PLACEMENTS as u64
                {
                    request.to = position;
                    continue;
                }
                if requests.len() == MAX_REQUESTS {
                    return Ok(requests);
                }
                requests.push(PlacementRequest {
                    seal: id,
                    author,
                    from: position,
                    to: position,
                });
            }
        }
        Ok(requests)
    }
    /// Answers `request` from the frames and voids this replica holds:
    /// released positions through the seal's frontier, without bodies.
    async fn placement_page(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        proved: &BTreeSet<ResourceScope>,
        request: PlacementRequest,
    ) -> Result<PlacementPage, Error> {
        let time = now()?;
        self.admit_chain(peer, credentials, time)?;
        if self
            .config
            .revocations
            .revoking(credentials)
            .map_err(credential)?
            .is_some()
        {
            return Err(denied(crate::network::PEER_REVOKED));
        }
        if self.private_schema().await? {
            return Err(denied("schema publication is private"));
        }
        let Some((domain, seal)) = self.graphs.iter().find_map(|(domain, graph)| {
            graph
                .seal_by_id(request.seal)
                .ok()
                .map(|seal| (*domain, seal.clone()))
        }) else {
            // A peer that has not learned the seal has nothing to place.
            return Ok(PlacementPage::Headers {
                headers: Vec::new(),
                chains: Vec::new(),
            });
        };
        let (grants, selectors) = credentials
            .verify_selectors(self.config.publisher, peer, &self.config.revocations, time)
            .map_err(credential)?;
        if !grants.iter().any(|grant| grant.scope.domain == domain)
            && !selectors.iter().any(|grant| grant.domain == domain)
        {
            return Err(denied("placement requester is not a domain member"));
        }
        let mark = seal
            .statement
            .frontier
            .iter()
            .find(|mark| mark.author == request.author)
            .map(|mark| mark.sequence);
        if request.from > request.to
            || request.to - request.from >= MAX_PLACEMENTS as u64
            || mark.is_none_or(|mark| request.to > mark)
        {
            return Err(denied("placement request outside the seal's frontier"));
        }
        let staged = self.staged().await?;
        let mut headers = BTreeSet::new();
        let mut chains = BTreeMap::new();
        for record in rows(
            &self.conn,
            "SELECT header,body FROM secure_operations UNION ALL SELECT header,X'' FROM secure_voids",
        )
        .await?
        {
            let [SqlValue::Blob(header), SqlValue::Blob(body)] = record.as_slice() else {
                return Err(Error::Corrupt("placement header".into()));
            };
            let decoded: p2panda_core::Header<Extensions> = decode(header)?;
            let e = &decoded.extensions;
            if e.scope.domain != domain
                || e.author != Some(request.author)
                || !(request.from..=request.to).contains(&e.author_sequence)
            {
                continue;
            }
            if staged.contains(&decoded.hash()) {
                continue;
            }
            // A migration frame leaves only with its released publication.
            if !body.is_empty()
                && !matches!(
                    self.migration_membership(decoded.hash()).await,
                    Ok(None | Some(MigrationMembership { descriptor: Some(_), .. }))
                )
            {
                continue;
            }
            // A revoked author's header is no evidence, as in range serving;
            // it is checked against the current set before any other gate.
            let chain = self.stored_chain(e.chain).await?;
            if self
                .config
                .revocations
                .revoking(chain.credentials())
                .map_err(credential)?
                .is_some()
            {
                continue;
            }
            if e.partition.is_some() && !proved.contains(&e.scope) {
                return Ok(PlacementPage::CustodyRequired);
            }
            if headers.len() == MAX_PLACEMENTS {
                break;
            }
            chains
                .entry(e.chain)
                .or_insert_with(|| chain.credentials().clone());
            headers.insert(Placement(header.clone()));
        }
        Ok(PlacementPage::Headers {
            headers: headers.into_iter().collect(),
            chains: chains.into_values().collect(),
        })
    }
    /// Verifies and stores `placements` answering `request`, inside one
    /// transaction. A header of a scope this replica holds is not stored:
    /// that scope needs the frame itself. Returns how many it stored.
    async fn receive_placements(
        &mut self,
        request: PlacementRequest,
        placements: Vec<Placement>,
        chains: Vec<CredentialChain>,
    ) -> Result<usize, Error> {
        if placements.len() > MAX_PLACEMENTS || chains.len() > MAX_PLACEMENTS {
            return Err(denied("placement page quota"));
        }
        let chains = chains
            .into_iter()
            .map(|credentials| {
                crate::wire::Chain::new(credentials).map(|chain| (chain.digest(), chain))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let time = now()?;
        let (domain, seal) = self
            .graphs
            .iter()
            .find_map(|(domain, graph)| {
                graph
                    .seal_by_id(request.seal)
                    .ok()
                    .map(|seal| (*domain, seal.clone()))
            })
            .ok_or_else(|| denied("unknown seal"))?;
        let mark = seal
            .statement
            .frontier
            .iter()
            .find(|mark| mark.author == request.author)
            .map(|mark| mark.sequence)
            .ok_or_else(|| denied("placement outside the seal's frontier"))?;
        let held = self.custody_scopes(time)?;
        let mut verified = Vec::new();
        for Placement(bytes) in placements {
            let header = crate::wire::verified_header(&bytes)?;
            let e = &header.extensions;
            let chain = chains
                .get(&e.chain)
                .ok_or_else(|| denied(crate::wire::INCOMPLETE_FRAME))?;
            if e.scope.domain != domain
                || e.author != Some(request.author)
                || !(request.from..=request.to.min(mark)).contains(&e.author_sequence)
            {
                return Err(denied("placement outside its request"));
            }
            if !self.visible_frame(
                self.graph_for(domain)?,
                e.tip,
                request.author,
                e.author_sequence,
                seal.statement.tip,
            )? {
                return Err(denied("placement not inside the seal's state"));
            }
            self.verify_header_authority(&header, chain.credentials(), time)?;
            // A scope this node may take custody of needs the frame itself.
            if held.contains(&e.scope) {
                continue;
            }
            verified.push((header, bytes));
        }
        let mut stored = 0;
        transaction!(self, |inner| {
            stored = 0;
            for (header, bytes) in &verified {
                if inner.place(domain, header, bytes).await? {
                    stored += 1;
                }
            }
            Ok(())
        })?;
        Ok(stored)
    }
    /// Stores one verified placement unless a frame, void or placement
    /// holds its position already; another header there is equivocation.
    async fn place(
        &self,
        domain: Scope,
        header: &p2panda_core::Header<Extensions>,
        bytes: &[u8],
    ) -> Result<bool, Error> {
        let e = &header.extensions;
        let author = e
            .author
            .ok_or_else(|| denied("node author number missing"))?;
        let hash = header.hash();
        if let Some(known) = self
            .header_at(domain, author, e.author_sequence, hash)
            .await?
        {
            if known.hash() != hash {
                self.record_equivocation(domain, &known, header).await?;
            }
            return Ok(false);
        }
        self.no_frames_intake(header).await?;
        self.conn
            .execute(
                "INSERT OR IGNORE INTO secure_placements VALUES(?,?,?,?,?,?)",
                bindings(vec![
                    SqlValue::Blob(encode(&domain)?),
                    SqlValue::Integer(i64::from(author)),
                    SqlValue::Integer(e.author_sequence as i64),
                    SqlValue::Blob(encode(&e.scope)?),
                    SqlValue::Blob(hash.as_bytes().to_vec()),
                    SqlValue::Blob(bytes.to_vec()),
                ]),
            )
            .await?;
        self.custody_required
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&(domain, author, e.author_sequence));
        Ok(true)
    }
    /// Prefers a conflicting retained header when the incoming placement is known.
    async fn header_at(
        &self,
        domain: Scope,
        author: u32,
        sequence: u64,
        incoming: Hash,
    ) -> Result<Option<p2panda_core::Header<Extensions>>, Error> {
        let records = collect_rows(
            self.conn
                .query(
                    "SELECT header FROM secure_placements WHERE domain=? AND author=? AND sequence=?",
                    bindings(vec![
                        SqlValue::Blob(encode(&domain)?),
                        SqlValue::Integer(i64::from(author)),
                        SqlValue::Integer(sequence as i64),
                    ]),
                )
                .await?,
        )
        .await?;
        if let Some([SqlValue::Blob(bytes)]) = records.first().map(Vec::as_slice) {
            return Ok(Some(decode(bytes)?));
        }
        let mut known = None;
        for record in rows(
            &self.conn,
            "SELECT header FROM secure_operations UNION ALL SELECT header FROM secure_voids",
        )
        .await?
        {
            let [SqlValue::Blob(bytes)] = record.as_slice() else {
                return Err(Error::Corrupt("frame header".into()));
            };
            let header: p2panda_core::Header<Extensions> = decode(bytes)?;
            let e = &header.extensions;
            // An equal match must not hide evidence of another signed header.
            if e.scope.domain == domain
                && e.author == Some(author)
                && e.author_sequence == sequence
                && known
                    .as_ref()
                    .is_none_or(|prior: &p2panda_core::Header<Extensions>| {
                        prior.hash() == incoming && header.hash() != incoming
                    })
            {
                known = Some(header);
            }
        }
        Ok(known)
    }
    #[cfg(test)]
    pub(crate) async fn place_verified_for_test(
        &self,
        header: &p2panda_core::Header<Extensions>,
    ) -> Result<bool, Error> {
        self.place(header.extensions.scope.domain, header, &header.to_bytes())
            .await
    }
    /// Inside the caller's transaction: a frame stored at a placed position
    /// replaces its placement; a different header there is equivocation.
    pub(super) async fn settle_placement(
        &self,
        header: &p2panda_core::Header<Extensions>,
    ) -> Result<(), Error> {
        let e = &header.extensions;
        let Some(author) = e.author else {
            return Ok(());
        };
        let domain = e.scope.domain;
        let records = collect_rows(
            self.conn
                .query(
                    "SELECT header FROM secure_placements WHERE domain=? AND author=? AND sequence=?",
                    bindings(vec![
                        SqlValue::Blob(encode(&domain)?),
                        SqlValue::Integer(i64::from(author)),
                        SqlValue::Integer(e.author_sequence as i64),
                    ]),
                )
                .await?,
        )
        .await?;
        let Some([SqlValue::Blob(bytes)]) = records.first().map(Vec::as_slice) else {
            return Ok(());
        };
        let placed: p2panda_core::Header<Extensions> = decode(bytes)?;
        if placed.hash() != header.hash() {
            self.record_equivocation(domain, &placed, header).await?;
        }
        self.conn
            .execute(
                "DELETE FROM secure_placements WHERE domain=? AND author=? AND sequence=?",
                bindings(vec![
                    SqlValue::Blob(encode(&domain)?),
                    SqlValue::Integer(i64::from(author)),
                    SqlValue::Integer(e.author_sequence as i64),
                ]),
            )
            .await?;
        Ok(())
    }
    async fn record_equivocation(
        &self,
        domain: Scope,
        first: &p2panda_core::Header<Extensions>,
        second: &p2panda_core::Header<Extensions>,
    ) -> Result<(), Error> {
        let e = &first.extensions;
        self.conn
            .execute(
                "INSERT OR IGNORE INTO secure_equivocations VALUES(?,?,?,?,?)",
                bindings(vec![
                    SqlValue::Blob(encode(&domain)?),
                    SqlValue::Integer(i64::from(e.author.unwrap_or_default())),
                    SqlValue::Integer(e.author_sequence as i64),
                    SqlValue::Blob(encode(first)?),
                    SqlValue::Blob(encode(second)?),
                ]),
            )
            .await?;
        Ok(())
    }
    /// Inside the caller's transaction: drops placements every held
    /// scope's base folded, through the oldest base seal of the path.
    pub(super) async fn prune_placements(&self, domain: Scope) -> Result<(), Error> {
        let path = self.path_seals(domain)?;
        let mut oldest: Option<usize> = None;
        for (_, id) in self.domain_scopes(domain).await? {
            let at = match self.base_seal(id).await? {
                Some(base) => self.path_index(domain, base)?,
                None => None,
            };
            let Some(at) = at else {
                return Ok(());
            };
            oldest = Some(oldest.map_or(at, |oldest| oldest.min(at)));
        }
        let Some(seal) = oldest.and_then(|at| path.get(at)) else {
            return Ok(());
        };
        for mark in &seal.statement.frontier {
            self.conn
                .execute(
                    "DELETE FROM secure_placements WHERE domain=? AND author=? AND sequence<=?",
                    bindings(vec![
                        SqlValue::Blob(encode(&domain)?),
                        SqlValue::Integer(i64::from(mark.author)),
                        SqlValue::Integer(mark.sequence as i64),
                    ]),
                )
                .await?;
        }
        Ok(())
    }
    /// Why a seal's positions of `domain` are missing: a peer showed that
    /// only a partition's custody can place one, or they are not here yet.
    pub(super) fn missing_placement(&self, domain: Scope) -> CoordinatedError {
        if self
            .custody_required
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|(scope, _, _)| *scope == domain)
        {
            CoordinatedError::CustodyRequired
        } else {
            CoordinatedError::Behind
        }
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// The placement requests this replica wants answered.
    pub async fn wanted_placements(&self) -> Result<Vec<PlacementRequest>, Error> {
        let inner = self.inner.lock().await;
        let Some((domain, _)) = inner.workspace.followed.clone() else {
            return Ok(Vec::new());
        };
        inner.wanted_placements(domain).await
    }
    /// Answers a peer's placement request.
    pub async fn placement_page(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        proved: &BTreeSet<ResourceScope>,
        request: PlacementRequest,
    ) -> Result<PlacementPage, Error> {
        self.inner
            .lock()
            .await
            .placement_page(peer, credentials, proved, request)
            .await
    }
    /// Stores a peer's answer to `request`; a page the peer could not
    /// answer without showing a partition marks its positions as needing
    /// that partition's custody.
    pub async fn receive_placements(
        &self,
        request: PlacementRequest,
        page: PlacementPage,
    ) -> Result<usize, Error> {
        let mut inner = self.inner.lock().await;
        match page {
            PlacementPage::Headers { headers, chains } => {
                let stored = inner.receive_placements(request, headers, chains).await?;
                if stored > 0 {
                    inner.publish_revision().await?;
                }
                Ok(stored)
            }
            PlacementPage::CustodyRequired => {
                let domain = inner
                    .graphs
                    .iter()
                    .find_map(|(domain, graph)| {
                        graph.seal_by_id(request.seal).ok().map(|_| *domain)
                    })
                    .ok_or_else(|| denied("unknown seal"))?;
                let mut blocked = inner
                    .custody_required
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                for sequence in request.from..=request.to {
                    blocked.insert((domain, request.author, sequence));
                }
                Ok(0)
            }
        }
    }
    /// Pairs of different signed headers seen at one node position.
    pub async fn equivocations(&self) -> Result<usize, Error> {
        let inner = self.inner.lock().await;
        match rows(&inner.conn, "SELECT count(*) FROM secure_equivocations")
            .await?
            .first()
            .map(Vec::as_slice)
        {
            Some([SqlValue::Integer(count)]) => Ok(*count as usize),
            _ => Err(Error::Corrupt("equivocations".into())),
        }
    }
}
