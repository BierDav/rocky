use crate::{Error, PeerId, PersonalLink, PublicKey, SignedCapability, denied};
use rocky_core::Commitment;
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_MEMBERSHIP_NODES: usize = crate::MAX_CUT_LANES;

pub(crate) fn node_database(
    certificate: &SignedCapability,
) -> Result<crate::DatabaseContext, Error> {
    match PersonalLink::of(certificate)? {
        Some(link) => Ok(link.database),
        None => MembershipLink::from_certificate(certificate).map(|link| link.database),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MembershipLink {
    pub database: crate::DatabaseContext,
    pub identity: PublicKey,
    pub node: PeerId,
    pub sequence: u64,
    pub previous: Option<Commitment>,
    pub certificate: Commitment,
}

impl MembershipLink {
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
            database: extended
                .database
                .ok_or_else(|| denied("membership certificate has no database"))?,
            identity: certificate.issuer,
            node,
            sequence,
            previous: extended.previous,
            certificate: certificate.id()?,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MembershipSet {
    database: Option<crate::DatabaseContext>,
    links: BTreeMap<(PublicKey, u64), Vec<MembershipLink>>,
    devices: BTreeMap<(PublicKey, PublicKey), BTreeSet<Commitment>>,
}

impl MembershipSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&mut self, certificate: &SignedCapability) -> Result<(), Error> {
        match PersonalLink::of(certificate)? {
            Some(link) => self.insert_personal(link),
            None => self.insert(certificate).map(drop),
        }
    }
    pub fn insert_personal(&mut self, link: PersonalLink) -> Result<(), Error> {
        self.bind(link.database)?;
        self.devices
            .entry((link.owner, link.node))
            .or_default()
            .insert(link.certificate);
        Ok(())
    }
    fn bind(&mut self, database: crate::DatabaseContext) -> Result<(), Error> {
        if self.database.is_some_and(|held| held != database) {
            return Err(denied("membership evidence belongs to another database"));
        }
        self.database = Some(database);
        Ok(())
    }
    pub fn devices(&self, owner: PublicKey) -> impl Iterator<Item = PublicKey> + '_ {
        self.devices
            .keys()
            .filter(move |(key, _)| *key == owner)
            .map(|(_, node)| *node)
    }

    pub fn insert(&mut self, certificate: &SignedCapability) -> Result<MembershipLink, Error> {
        let link = MembershipLink::from_certificate(certificate)?;
        self.bind(link.database)?;
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

    pub fn accepts(&self, certificate: Commitment) -> bool {
        self.devices.values().any(|ids| ids.contains(&certificate))
            || self
                .valid_links()
                .any(|link| link.certificate == certificate)
    }

    fn valid_links(&self) -> impl Iterator<Item = &MembershipLink> {
        let mut heads = BTreeMap::new();
        self.links.iter().filter_map(move |((identity, _), links)| {
            if links.len() != 1 {
                return None;
            }
            let link = &links[0];
            let previous = heads.get(identity).copied();
            if link.previous != previous {
                return None;
            }
            heads.insert(*identity, link.certificate);
            Some(link)
        })
    }

    pub fn is_valid(&self, identity: PublicKey) -> bool {
        !self.is_forked(identity)
            && self
                .links(identity)
                .all(|link| self.accepts(link.certificate))
    }

    pub fn head(&self, identity: PublicKey) -> Option<MembershipLink> {
        self.valid_links()
            .filter(|link| link.identity == identity)
            .last()
            .copied()
    }

    pub fn next_sequence(&self, identity: PublicKey) -> Result<u64, Error> {
        self.links(identity)
            .map(|link| link.sequence)
            .max()
            .map_or(Ok(0), |sequence| {
                sequence
                    .checked_add(1)
                    .ok_or_else(|| denied("membership sequence exhausted"))
            })
    }

    pub(crate) fn void_certificates(&self) -> BTreeSet<Commitment> {
        let mut void: BTreeSet<_> = self
            .links
            .values()
            .filter(|links| links.len() > 1)
            .flatten()
            .map(|link| link.certificate)
            .collect();
        loop {
            let mut changed = false;
            for link in self.links.values().flatten() {
                if link
                    .previous
                    .is_some_and(|previous| void.contains(&previous))
                {
                    changed |= void.insert(link.certificate);
                }
            }
            if !changed {
                return void;
            }
        }
    }
    pub fn can_link(&self, identity: PublicKey, unlinked: impl Fn(PeerId) -> bool) -> bool {
        let void = self.void_certificates();
        self.links(identity)
            .filter(|link| !void.contains(&link.certificate) && !unlinked(link.node))
            .map(|link| link.node)
            .collect::<BTreeSet<_>>()
            .len()
            < MAX_MEMBERSHIP_NODES
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
        let database = crate::DatabaseContext {
            publisher: identity.public_key(),
            scope: rocky_core::Scope {
                mesh: rocky_core::MeshId([1; 16]),
                app: rocky_core::AppId([2; 16]),
                domain: rocky_core::DomainId([3; 16]),
            },
        };
        let capability = Capability {
            subject: node.public_key(),
            kind: rocky_core::PrincipalKind::Node,
            grants: Vec::new(),
            not_before: 1,
            expires: crate::NEVER,
            delegations: 0,
            extension: Extension::new(Extended {
                node: Some(PeerId::of(node.public_key())),
                sequence: Some(sequence),
                previous,
                database: Some(database),
                possession: Some(
                    crate::NodePossession::sign(node, identity.public_key(), database, None)
                        .unwrap(),
                ),
                ..Extended::default()
            })
            .unwrap(),
        };
        SignedCapability::sign(capability, identity).unwrap()
    }

    #[test]
    fn changing_unsigned_envelope_fields_cannot_forge_a_membership_fork() {
        let identity = PrivateKey::new();
        let first = certificate(&identity, &PrivateKey::new(), 0, None);
        let other = certificate(&identity, &PrivateKey::new(), 0, None);
        let mut forged = first.clone();
        forged.author_binding = other.author_binding;
        assert_eq!(
            forged.capability, first.capability,
            "the capability's signature survives the forgery"
        );
        assert_eq!(forged.attestation(), first.attestation());
        let mut set = crate::RevocationSet::new();
        set.link(first).unwrap();
        let before = set.clone();
        assert!(set.link(forged).is_err());
        assert_eq!(set, before);
        assert!(!set.membership().unwrap().is_forked(identity.public_key()));
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
        assert!(!bounded.can_link(identity.public_key(), |_| false));
        let first_node = bounded.links(identity.public_key()).next().unwrap().node;
        assert!(bounded.can_link(identity.public_key(), |node| node == first_node));
        bounded.insert(&extra).unwrap();
        assert_eq!(bounded.next_sequence(identity.public_key()).unwrap(), 33);
        let _ = next_link;
    }
}
