//! Admission of a session peer by its chain, and the bound on what
//! unconfigured peers leave behind
//! (`docs/decisions/0051-roster-free-admission.md`).
use super::*;
use rocky_credentials::PeerId;

/// Unconfigured peers whose horizon credentials and frontiers a replica
/// keeps; the least recently served beyond it lose theirs.
pub const MAX_UNCONFIGURED_PEERS: usize = 256;
/// Session peers whose declared tip a replica remembers; the least
/// recently served beyond it count as running this replica's generation.
const MAX_PEER_TIPS: usize = 4 * MAX_UNCONFIGURED_PEERS;

impl<A: RowApplication> Inner<A> {
    /// The chain check of a session peer, before any revocation: an
    /// unexpired node chain under the publisher granting only this
    /// application. A chain names no generation; peers of every
    /// generation present the same one.
    fn admit_chain(
        &self,
        node: PublicKey,
        chain: &CredentialChain,
        time: u64,
    ) -> Result<(), Error> {
        if chain
            .0
            .last()
            .is_none_or(|leaf| leaf.capability.kind != rocky_core::PrincipalKind::Node)
        {
            return Err(denied("a session requires a node credential"));
        }
        let (grants, selectors) = chain
            .verify_selectors(self.config.publisher, node, &RevocationSet::new(), time)
            .map_err(credential)?;
        let app = self.definition.app;
        if grants.iter().any(|grant| grant.scope.domain.app != app)
            || selectors.iter().any(|grant| grant.domain.app != app)
        {
            return Err(denied("credentials for another domain"));
        }
        Ok(())
    }
    /// Records the tip `peer` declared when its session opened.
    fn record_peer_tip(&mut self, peer: PublicKey, tip: TipId) {
        self.peer_tips.insert(PeerId::of(peer), tip);
        if self.peer_tips.len() > MAX_PEER_TIPS
            && let Some(oldest) = self
                .peer_tips
                .keys()
                .min_by_key(|peer| self.served.1.get(*peer).copied().unwrap_or(0))
                .copied()
        {
            self.peer_tips.remove(&oldest);
        }
    }
    /// The declared session tip. Without a graph, direct callers use genesis;
    /// once tips exist a missing or evicted declaration fails closed.
    pub(crate) fn peer_tip(&self, peer: PublicKey) -> Option<TipId> {
        self.peer_tips
            .get(&PeerId::of(peer))
            .copied()
            .or_else(|| self.graphs.is_empty().then_some(self.tip))
    }
    /// Marks `peer` served now.
    pub(super) fn touch_peer(&mut self, peer: PublicKey) {
        self.served.0 += 1;
        let at = self.served.0;
        self.served.1.insert(PeerId::of(peer), at);
    }
    /// Whether `peer` has no row in either bounded table yet.
    pub(super) async fn unrecorded_peer(&self, peer: PublicKey) -> Result<bool, Error> {
        let key = PeerId::of(peer).as_bytes().to_vec();
        let mut found = self
            .conn
            .query(
                "SELECT 1 FROM secure_horizon_credentials WHERE node=?1 UNION ALL SELECT 1 FROM secure_peer_frontiers WHERE peer=?1 LIMIT 1",
                [key],
            )
            .await?;
        Ok(found.next().await?.is_none())
    }
    /// Inside the open transaction, after a peer's first row: removes the
    /// rows of the least recently served unconfigured peers beyond the
    /// bound. Configured peers and full nodes are never removed. A lost
    /// frontier only delays pruning, lost credentials a horizon deadline.
    pub(super) async fn bound_peers(&mut self) -> Result<(), Error> {
        let full = self.role().await?.full_nodes;
        let configured: BTreeSet<_> = self.configured.iter().copied().map(PeerId::of).collect();
        let mut unconfigured = Vec::new();
        for record in rows(
            &self.conn,
            "SELECT node FROM secure_horizon_credentials UNION SELECT peer FROM secure_peer_frontiers",
        )
        .await?
        {
            let [SqlValue::Blob(peer)] = record.as_slice() else {
                return Err(Error::Corrupt("peer key".into()));
            };
            let peer = PeerId::from_bytes(peer.as_slice().try_into().map_err(|_| Error::Corrupt("peer reference".into()))?);
            if !configured.contains(&peer) && !full.contains(&peer) {
                unconfigured.push(peer);
            }
        }
        if unconfigured.len() <= MAX_UNCONFIGURED_PEERS {
            return Ok(());
        }
        let excess = unconfigured.len() - MAX_UNCONFIGURED_PEERS;
        unconfigured.sort_by_key(|peer| self.served.1.get(peer).copied().unwrap_or(0));
        for peer in &unconfigured[..excess] {
            let key = peer.as_bytes().to_vec();
            self.conn
                .execute(
                    "DELETE FROM secure_horizon_credentials WHERE node=?",
                    [key.clone()],
                )
                .await?;
            self.conn
                .execute("DELETE FROM secure_peer_frontiers WHERE peer=?", [key])
                .await?;
            self.served.1.remove(peer);
        }
        Ok(())
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// Fails unless `chain` admits `node` to a session with this replica,
    /// revocations aside; nothing is written.
    pub(crate) async fn admit_chain(
        &self,
        node: PublicKey,
        chain: &CredentialChain,
    ) -> Result<(), Error> {
        self.inner.lock().await.admit_chain(node, chain, now()?)
    }
    /// `admit_chain` for a session that opens, remembering the tip
    /// `manifest` the peer declared, which decides what it is sent.
    pub(crate) async fn admit_peer(
        &self,
        node: PublicKey,
        chain: &CredentialChain,
        tip: TipId,
    ) -> Result<(), Error> {
        let mut inner = self.inner.lock().await;
        inner.admit_chain(node, chain, now()?)?;
        inner.record_peer_tip(node, tip);
        Ok(())
    }
    /// The peers whose credentials and frontiers stay whatever the bound on
    /// unconfigured peers: the contacts this device keeps.
    pub async fn configure_peers(&self, peers: impl IntoIterator<Item = PublicKey>) {
        self.inner.lock().await.configured = peers.into_iter().collect();
    }
    /// Distinct peers with horizon credentials and with frontiers.
    #[cfg(test)]
    pub(crate) async fn recorded_peers(&self) -> (usize, usize) {
        let inner = self.inner.lock().await;
        let count = async |sql| rows(&inner.conn, sql).await.unwrap().len();
        (
            count("SELECT node FROM secure_horizon_credentials").await,
            count("SELECT DISTINCT peer FROM secure_peer_frontiers").await,
        )
    }
}
