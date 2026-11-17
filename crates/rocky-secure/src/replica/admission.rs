use super::*;
use rocky_credentials::PeerId;

pub const MAX_UNCONFIGURED_PEERS: usize = 256;
const MAX_PEER_TIPS: usize = 4 * MAX_UNCONFIGURED_PEERS;

impl<A: RowApplication> Inner<A> {
    pub(super) fn admit_chain(
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
        chain
            .verify_database(
                self.config
                    .credentials
                    .database_context(self.config.publisher)
                    .map_err(credential)?,
            )
            .map_err(credential)?;
        let pinned = self.config.revocations.pinned_only();
        let (grants, selectors) = chain
            .verified_with_families(self.config.publisher, &pinned, time, self.grant_families())
            .map_err(credential)?
            .selectors(node, &pinned, time)
            .map_err(credential)?;
        let app = self.definition.app;
        if grants.iter().any(|grant| grant.scope.domain.app != app)
            || selectors.iter().any(|grant| grant.domain.app != app)
        {
            return Err(denied("credentials for another domain"));
        }
        Ok(())
    }
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
    pub(crate) fn peer_tip(&self, peer: PublicKey) -> Option<TipId> {
        self.peer_tips.get(&PeerId::of(peer)).copied().or_else(|| {
            self.graphs
                .values()
                .all(|graph| graph.seals.is_empty() && graph.openings.is_empty())
                .then_some(self.workspace.tip)
        })
    }
    pub(super) fn touch_peer(&mut self, peer: PublicKey) {
        self.served.0 += 1;
        let at = self.served.0;
        self.served.1.insert(PeerId::of(peer), at);
    }
    pub(super) async fn unrecorded_peer(&self, peer: PublicKey) -> Result<bool, Error> {
        let key = PeerId::of(peer).as_bytes().to_vec();
        let mut found = self
            .conn
            .query(
                "SELECT 1 FROM secure_peer_frontiers WHERE peer=?1 LIMIT 1",
                [key],
            )
            .await?;
        Ok(found.next().await?.is_none())
    }
    pub(super) async fn bound_peers(&mut self) -> Result<(), Error> {
        let full = self.custody_peers().await?;
        let configured: BTreeSet<_> = self.configured.iter().copied().map(PeerId::of).collect();
        let mut unconfigured = Vec::new();
        for record in rows(
            &self.conn,
            "SELECT DISTINCT peer FROM secure_peer_frontiers",
        )
        .await?
        {
            let [SqlValue::Blob(peer)] = record.as_slice() else {
                return Err(Error::Corrupt("peer key".into()));
            };
            let peer = PeerId::from_bytes(
                peer.as_slice()
                    .try_into()
                    .map_err(|_| Error::Corrupt("peer reference".into()))?,
            );
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
                .execute("DELETE FROM secure_peer_frontiers WHERE peer=?", [key])
                .await?;
            self.served.1.remove(peer);
        }
        Ok(())
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    pub(crate) async fn admit_chain(
        &self,
        node: PublicKey,
        chain: &CredentialChain,
    ) -> Result<(), Error> {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .admit_chain(node, chain, now()?)
    }
    pub(crate) async fn admit_peer(
        &self,
        node: PublicKey,
        chain: &CredentialChain,
        tip: TipId,
    ) -> Result<(), Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        inner.admit_chain(node, chain, now()?)?;
        inner.record_peer_tip(node, tip);
        Ok(())
    }
    pub async fn configure_peers(&self, peers: impl IntoIterator<Item = PublicKey>) {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .configured = peers.into_iter().collect();
    }
    #[cfg(test)]
    pub(crate) async fn recorded_peers(&self) -> usize {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        rows(
            &inner.conn,
            "SELECT DISTINCT peer FROM secure_peer_frontiers",
        )
        .await
        .unwrap()
        .len()
    }
}
