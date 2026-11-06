//! Identity membership links and their deterministic fork view.
use crate::{Error, PeerId, PublicKey, SignedCapability, denied};
use rocky_core::Commitment;
use std::collections::BTreeMap;

/// The largest number of simultaneously linked nodes an identity may hold.
pub const MAX_MEMBERSHIP_NODES: usize = crate::MAX_CUT_LANES;

/// The membership metadata carried by one node certificate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MembershipLink {
    pub identity: PublicKey,
    pub node: PeerId,
    pub sequence: u64,
    pub previous: Option<Commitment>,
    pub certificate: Commitment,
}

impl MembershipLink {
    /// Extracts and validates a certificate's membership fields.
    pub fn from_certificate(certificate: &SignedCapability) -> Result<Self, Error> {
        let capability = &certificate.capability;
        let extended = capability.extended()?;
        let node = extended
            .node
            .ok_or_else(|| denied("membership link is not a node certificate"))?;
        let sequence = extended
            .sequence
            .ok_or_else(|| denied("node certificate has no membership sequence"))?;
        if (sequence == 0) != extended.previous.is_none()
            || capability.kind != rocky_core::PrincipalKind::Node
            || capability.delegations != 0
        {
            return Err(denied("invalid membership certificate"));
        }
        Ok(Self {
            identity: certificate.issuer,
            node,
            sequence,
            previous: extended.previous,
            certificate: certificate.id()?,
        })
    }
}

/// A deterministic view of all membership links known to a node.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MembershipSet {
    links: BTreeMap<(PublicKey, u64), Vec<MembershipLink>>,
}

impl MembershipSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts a certificate. A second certificate at one identity sequence
    /// is retained as a fork so every holder reaches the same equivocation
    /// result regardless of arrival order.
    pub fn insert(&mut self, certificate: &SignedCapability) -> Result<MembershipLink, Error> {
        let link = MembershipLink::from_certificate(certificate)?;
        let known_nodes = self
            .links(link.identity)
            .map(|known| known.node)
            .filter(|node| *node != link.node)
            .count();
        if known_nodes >= MAX_MEMBERSHIP_NODES {
            return Err(denied("identity membership limit reached"));
        }
        let entry = self
            .links
            .entry((link.identity, link.sequence))
            .or_default();
        if !entry
            .iter()
            .any(|known| known.certificate == link.certificate)
        {
            entry.push(link);
            entry.sort_by_key(|known| known.certificate);
        }
        Ok(link)
    }

    pub fn links(&self, identity: PublicKey) -> impl Iterator<Item = &MembershipLink> {
        self.links
            .iter()
            .filter(move |((key, _), _)| *key == identity)
            .flat_map(|(_, links)| links)
    }

    pub fn forks(&self, identity: PublicKey) -> impl Iterator<Item = (u64, &[MembershipLink])> {
        self.links
            .iter()
            .filter_map(move |((key, sequence), links)| {
                (*key == identity && links.len() > 1).then_some((*sequence, links.as_slice()))
            })
    }

    pub fn is_forked(&self, identity: PublicKey) -> bool {
        self.forks(identity).next().is_some()
    }

    /// Whether every known link forms one unambiguous predecessor chain.
    /// Forked sequences and missing predecessors are invalid until the
    /// identity links again from the surviving head.
    pub fn is_valid(&self, identity: PublicKey) -> bool {
        if self.is_forked(identity) {
            return false;
        }
        let mut previous = self
            .links(identity)
            .find(|link| link.sequence == 0)
            .map(|link| link.certificate);
        for link in self.links(identity).filter(|link| link.sequence > 0) {
            if link.previous != previous {
                return false;
            }
            previous = Some(link.certificate);
        }
        true
    }

    /// Returns the latest non-forked membership head, if one exists.
    pub fn head(&self, identity: PublicKey) -> Option<MembershipLink> {
        self.links(identity)
            .filter(|link| {
                !self
                    .links
                    .get(&(identity, link.sequence))
                    .is_some_and(|links| links.len() > 1)
            })
            .max_by_key(|link| link.sequence)
            .copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Capability, Extended, Extension, PrivateKey};

    fn certificate(
        identity: &PrivateKey,
        node: &PrivateKey,
        sequence: u64,
        previous: Option<Commitment>,
    ) -> SignedCapability {
        let capability = Capability {
            subject: node.public_key(),
            kind: rocky_core::PrincipalKind::Node,
            grants: Vec::new(),
            not_before: 1,
            expires: 2,
            delegations: 0,
            extension: Extension::new(Extended {
                node: Some(PeerId::of(node.public_key())),
                authors: Some(crate::AuthorRange::new(1, 1).unwrap()),
                sequence: Some(sequence),
                previous,
                ..Extended::default()
            })
            .unwrap(),
        };
        SignedCapability::sign(capability, identity).unwrap()
    }

    #[test]
    fn concurrent_links_at_one_sequence_are_a_stable_fork() {
        let identity = PrivateKey::new();
        let a = certificate(&identity, &PrivateKey::new(), 0, None);
        let b = certificate(&identity, &PrivateKey::new(), 0, None);
        let mut set = MembershipSet::new();
        set.insert(&b).unwrap();
        set.insert(&a).unwrap();
        assert!(set.is_forked(identity.public_key()));
        assert_eq!(set.forks(identity.public_key()).next().unwrap().0, 0);
        assert!(set.head(identity.public_key()).is_none());
    }

    #[test]
    fn a_link_must_follow_the_membership_head_and_the_bound_is_32_nodes() {
        let identity = PrivateKey::new();
        let first = certificate(&identity, &PrivateKey::new(), 0, None);
        let mut set = MembershipSet::new();
        let head = set.insert(&first).unwrap();
        let next = certificate(&identity, &PrivateKey::new(), 1, Some(head.certificate));
        let next_link = set.insert(&next).unwrap();
        assert!(set.is_valid(identity.public_key()));
        let wrong = certificate(&identity, &PrivateKey::new(), 2, Some(first.id().unwrap()));
        set.insert(&wrong).unwrap();
        assert!(!set.is_valid(identity.public_key()));
        let mut bounded = MembershipSet::new();
        for sequence in 0..MAX_MEMBERSHIP_NODES as u64 {
            let prior = if sequence == 0 {
                None
            } else {
                bounded
                    .head(identity.public_key())
                    .map(|link| link.certificate)
            };
            let cert = certificate(&identity, &PrivateKey::new(), sequence, prior);
            bounded.insert(&cert).unwrap();
        }
        let extra = certificate(
            &identity,
            &PrivateKey::new(),
            32,
            bounded
                .head(identity.public_key())
                .map(|link| link.certificate),
        );
        assert!(bounded.insert(&extra).is_err());
        let _ = next_link;
    }
}
