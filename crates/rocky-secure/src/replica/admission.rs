//! Admission of a session peer by its chain, and the bound on what
//! unconfigured peers leave behind
//! (`docs/decisions/0051-roster-free-admission.md`).
use super::checkpoint::key_of;
use super::*;

/// Unconfigured peers whose horizon credentials and frontiers a replica
/// keeps; the least recently served beyond it lose theirs.
pub const MAX_UNCONFIGURED_PEERS: usize = 256;

impl<A: RowApplication> Inner<A> {
    /// The chain check of a session peer, before any revocation: a node
    /// chain under the publisher, unexpired, for a generation this replica
    /// admits. A newer generation's chain, which this replica cannot read
    /// yet, is checked under its own manifest and must grant only this
    /// application.
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
        let manifest = chain
            .0
            .first()
            .map_or(self.manifest, |root| root.capability.manifest);
        let newer = match self.admits(manifest) {
            Ok(_) => false,
            Err(error) if self.known(manifest).is_ok() => return Err(error),
            Err(_) => true,
        };
        let (grants, selectors) = chain
            .verify_selectors(
                self.config.publisher,
                manifest,
                node,
                &RevocationSet::new(),
                time,
            )
            .map_err(credential)?;
        let app = self.definition.app;
        if newer
            && (grants.iter().any(|grant| grant.scope.domain.app != app)
                || selectors.iter().any(|grant| grant.domain.app != app))
        {
            return Err(denied("credentials for another domain"));
        }
        Ok(())
    }
    /// Marks `peer` served now.
    pub(super) fn touch_peer(&mut self, peer: PublicKey) {
        self.served.0 += 1;
        let at = self.served.0;
        self.served.1.insert(peer, at);
    }
    /// Whether `peer` has no row in either bounded table yet.
    pub(super) async fn unrecorded_peer(&self, peer: PublicKey) -> Result<bool, Error> {
        let key = peer.as_bytes().to_vec();
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
            let peer = key_of(peer)?;
            if !self.configured.contains(&peer) && !full.contains(&peer) {
                unconfigured.push(peer);
            }
        }
        let Some(excess) = unconfigured.len().checked_sub(MAX_UNCONFIGURED_PEERS) else {
            return Ok(());
        };
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
    /// Admits a command carrier's node under the held generation horizon and
    /// revocation set, without requiring a configured contact or writing state.
    pub async fn admit_command_peer(
        &self,
        node: PublicKey,
        chain: &CredentialChain,
    ) -> Result<(), Error> {
        let inner = self.inner.lock().await;
        let time = now()?;
        inner.admit_chain(node, chain, time)?;
        inner.verify_chain_with(chain, node, &inner.config.revocations, time)?;
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
