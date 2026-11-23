use super::*;
use rocky_credentials::PeerId;

mod known_peers;
pub(crate) use known_peers::KnownPeers;

pub const MAX_UNCONFIGURED_PEERS: usize = 256;
pub(crate) const MAX_REMEMBERED_PEERS: usize = 4 * MAX_UNCONFIGURED_PEERS;

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
        let no_revocations = RevocationSet::default();
        let (grants, selectors) = chain
            .verified(
                self.config.publisher,
                &no_revocations,
                time,
                self.whole_tables(),
            )
            .map_err(credential)?
            .selectors(node, &no_revocations, time)
            .map_err(credential)?;
        let app = self.definition.app;
        if grants.iter().any(|grant| grant.scope.domain.app != app)
            || selectors.iter().any(|grant| grant.domain.app != app)
        {
            return Err(denied("credentials for another domain"));
        }
        Ok(())
    }
    pub(crate) fn peer_tip(&self, peer: PublicKey) -> Option<TipId> {
        self.known_peers.tip(&PeerId::of(peer)).or_else(|| {
            self.graphs
                .values()
                .all(|graph| graph.seals.is_empty() && graph.openings.is_empty())
                .then_some(self.workspace.tip)
        })
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
    pub(super) async fn bound_peers(&self) -> Result<(), Error> {
        let mut unkept = Vec::new();
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
            if !self.known_peers.kept(&peer) {
                unkept.push((self.known_peers.last_seen(&peer), peer));
            }
        }
        let excess = unkept.len().saturating_sub(MAX_UNCONFIGURED_PEERS);
        if excess == 0 {
            return Ok(());
        }
        unkept.sort_unstable();
        for (_, peer) in &unkept[..excess] {
            self.conn
                .execute(
                    "DELETE FROM secure_peer_frontiers WHERE peer=?",
                    [peer.as_bytes().to_vec()],
                )
                .await?;
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
        inner.known_peers.admit(PeerId::of(node), tip);
        Ok(())
    }
    pub async fn configure_peers(&self, peers: impl IntoIterator<Item = PublicKey>) {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .known_peers
            .configure(peers.into_iter().map(PeerId::of).collect());
    }
    #[cfg(test)]
    pub(crate) async fn admitted_tip(&self, peer: PublicKey) -> Option<TipId> {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .known_peers
            .tip(&PeerId::of(peer))
    }
    #[cfg(test)]
    pub(crate) async fn admitted_peers(&self) -> usize {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .known_peers
            .admitted()
            .count()
    }
    #[cfg(test)]
    pub(crate) async fn remembered_peers(&self) -> usize {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .known_peers
            .remembered()
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
