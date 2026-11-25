#[cfg(test)]
use rocky_core::PrincipalKind;
#[cfg(test)]
use rocky_credentials::CredentialChain;
use rocky_credentials::PublicKey;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, btree_map::Entry};

pub const MAX_SEEN: usize = crate::registry::MAX_IDENTITIES * 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkedNode {
    pub node: PublicKey,
    pub linked: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Seen {
    pub identity: PublicKey,
    pub linked: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceRevocationReceipt {
    pub node: rocky_credentials::PeerId,
    pub statement: rocky_core::Commitment,
    pub local: bool,
    pub delivered: std::collections::BTreeSet<rocky_credentials::PeerId>,
    pub pending: std::collections::BTreeSet<rocky_credentials::PeerId>,
}
pub fn see(seen: &mut BTreeMap<PublicKey, Seen>, identity: PublicKey, node: PublicKey, time: u64) {
    if let Some(held) = seen.get_mut(&node) {
        held.identity = identity;
        return;
    }
    if seen.len() >= MAX_SEEN
        && let Some(oldest) = seen
            .iter()
            .min_by_key(|(_, held)| held.linked)
            .map(|(node, _)| *node)
    {
        seen.remove(&oldest);
    }
    seen.insert(
        node,
        Seen {
            identity,
            linked: time,
        },
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Member {
    identity: PublicKey,
    node: LinkedNode,
}
#[derive(Clone, Debug, Default)]
pub struct Membership(BTreeMap<PublicKey, Member>);
impl Membership {
    #[cfg(test)]
    pub fn of<'a>(
        chains: impl IntoIterator<Item = &'a CredentialChain>,
        seen: &BTreeMap<PublicKey, Seen>,
    ) -> Self {
        let mut membership = Self::default();
        for (node, seen) in seen {
            membership.0.insert(
                *node,
                Member {
                    identity: seen.identity,
                    node: LinkedNode {
                        node: *node,
                        linked: seen.linked,
                    },
                },
            );
        }
        for chain in chains {
            let Some(leaf) = chain.0.last() else {
                continue;
            };
            if leaf.capability.kind != PrincipalKind::Node {
                continue;
            }
            let node = leaf.capability.subject;
            let linked = leaf.capability.not_before;
            membership
                .0
                .entry(node)
                .and_modify(|held| {
                    held.identity = leaf.issuer;
                    held.node.linked = held.node.linked.min(linked);
                })
                .or_insert(Member {
                    identity: leaf.issuer,
                    node: LinkedNode { node, linked },
                });
        }
        membership
    }
    pub(crate) fn replicated(
        set: &rocky_credentials::RevocationSet,
        database: rocky_credentials::DatabaseContext,
    ) -> Result<Self, crate::Failure> {
        set.verify_in(database)?;
        let mut view = Self::default();
        for certificate in set.certificates() {
            let (identity, node) = (certificate.issuer, certificate.capability.subject);
            if set.ends_node_of(identity, node, false) {
                continue;
            }
            let linked = certificate.capability.not_before;
            match view.0.entry(node) {
                Entry::Vacant(vacant) => {
                    vacant.insert(Member {
                        identity,
                        node: LinkedNode { node, linked },
                    });
                }
                Entry::Occupied(mut held) if held.get().identity == identity => {
                    let held = &mut held.get_mut().node;
                    held.linked = held.linked.min(linked);
                }
                Entry::Occupied(_) => {
                    return Err("a live node is certified by two identities".into());
                }
            }
        }
        Ok(view)
    }
    pub fn nodes(&self, identity: &PublicKey) -> impl Iterator<Item = &LinkedNode> {
        self.0
            .values()
            .filter(move |member| member.identity == *identity)
            .map(|member| &member.node)
    }
    #[cfg(test)]
    pub fn identity_of(&self, node: &PublicKey) -> Option<PublicKey> {
        self.0.get(node).map(|member| member.identity)
    }
    pub fn all(&self) -> impl Iterator<Item = (PublicKey, &LinkedNode)> {
        self.0
            .values()
            .map(|member| (member.identity, &member.node))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminObservation {
    pub linked: u64,
    pub unlinked: Option<u64>,
    pub read: bool,
    pub acknowledged: bool,
}
impl AdminObservation {
    pub(crate) fn alerts(
        &self,
        identity: PublicKey,
        node: PublicKey,
    ) -> impl Iterator<Item = crate::Alert> {
        let identity = crate::PersonId::new(identity);
        let node = crate::DeviceId::new(node);
        (!self.acknowledged)
            .then_some(crate::Alert {
                kind: crate::AlertKind::AdminDeviceLinked {
                    identity,
                    node,
                    linked: self.linked,
                },
                since: Some(self.linked),
            })
            .into_iter()
            .chain(self.unlinked.map(|unlinked| crate::Alert {
                kind: crate::AlertKind::AdminDeviceUnlinked {
                    identity,
                    node,
                    unlinked,
                },
                since: Some(unlinked),
            }))
    }
}

pub(crate) fn observe_admins(
    known: &rocky_credentials::RevocationSet,
    admins: &[PublicKey],
    time: u64,
    observed: &mut BTreeMap<PublicKey, BTreeMap<PublicKey, AdminObservation>>,
) -> Result<(), crate::Failure> {
    let membership = known.membership()?;
    for certificate in known.certificates().chain(
        known
            .statements()
            .filter_map(|signed| signed.chain.certificate()),
    ) {
        let identity = certificate.issuer;
        let node = certificate.capability.subject;
        if !admins.contains(&identity)
            || known.contests_node(node)
            || !membership.accepts(certificate.id()?)
        {
            continue;
        }
        let observation =
            observed
                .entry(identity)
                .or_default()
                .entry(node)
                .or_insert(AdminObservation {
                    linked: time,
                    unlinked: None,
                    read: false,
                    acknowledged: false,
                });
        if let Some(unlinked) = known
            .statements()
            .filter(|signed| {
                !signed.root()
                    && signed.statement.issuer == identity
                    && signed
                        .statement
                        .nodes
                        .contains(&rocky_credentials::PeerId::of(node))
            })
            .map(|signed| signed.statement.time)
            .min()
        {
            observation.unlinked = Some(
                observation
                    .unlinked
                    .map_or(unlinked, |prior| prior.min(unlinked)),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rocky_credentials::{Authority, PrivateKey, RevocationSet};

    const NOW: u64 = 1_800_000_000;

    fn certified(
        root: &PrivateKey,
        identity: &PrivateKey,
        node: &PrivateKey,
        previous: Option<&CredentialChain>,
    ) -> CredentialChain {
        let scope = rocky_core::Scope {
            mesh: rocky_core::MeshId([1; 16]),
            app: rocky_core::AppId([2; 16]),
            domain: rocky_core::DomainId([3; 16]),
        };
        let head = previous.map(|chain| chain.certificate().unwrap().id().unwrap());
        CredentialChain(vec![
            Authority::owner(root)
                .issue_trusted(identity.public_key(), scope, NOW)
                .unwrap(),
        ])
        .certify_member(identity, node, u64::from(head.is_some()), head)
        .unwrap()
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn a_node_key_belongs_to_at_most_one_identity_whatever_the_arrival_order() {
        let (root, a, b) = (PrivateKey::new(), PrivateKey::new(), PrivateKey::new());
        let (own, shared) = (PrivateKey::new(), PrivateKey::new());
        let a_own = certified(&root, &a, &own, None);
        let a_shared = certified(&root, &a, &shared, Some(&a_own));
        let b_shared = certified(&root, &b, &shared, None);
        let database = a_own.database_context(root.public_key()).unwrap();
        let certificates =
            [&a_own, &a_shared, &b_shared].map(|chain| chain.certificate().unwrap().clone());
        for order in [[0, 1, 2], [2, 1, 0], [1, 2, 0]] {
            let mut known = RevocationSet::new();
            for index in order {
                known.link(certificates[index].clone()).unwrap();
            }
            let membership = Membership::replicated(&known, database).unwrap();
            assert_eq!(
                membership.identity_of(&own.public_key()),
                Some(a.public_key())
            );
            assert_eq!(
                membership.identity_of(&shared.public_key()),
                None,
                "a node key two identities certify is attributed to neither"
            );
            assert_eq!(
                membership
                    .nodes(&a.public_key())
                    .map(|linked| linked.node)
                    .collect::<Vec<_>>(),
                [own.public_key()]
            );
            assert_eq!(membership.nodes(&b.public_key()).count(), 0);
        }
    }

    struct Shared {
        known: RevocationSet,
        identities: [PrivateKey; 4],
        nodes: [PrivateKey; 3],
    }
    fn shared_handed_and_own_nodes() -> Shared {
        let root = PrivateKey::new();
        let identities = [(); 4].map(|()| PrivateKey::new());
        let nodes = [(); 3].map(|()| PrivateKey::new());
        let [a, b, c, d] = &identities;
        let [own, shared, handed] = &nodes;
        let a_own = certified(&root, a, own, None);
        let c_handed = certified(&root, c, handed, None);
        let unlink = c_handed
            .unlink(c, rocky_core::AppId([2; 16]), NOW, handed.public_key())
            .unwrap();
        let mut known = RevocationSet::try_from(vec![unlink]).unwrap();
        for chain in [
            certified(&root, a, shared, Some(&a_own)),
            a_own,
            certified(&root, b, shared, None),
            certified(&root, d, handed, None),
        ] {
            known.link(chain.certificate().unwrap().clone()).unwrap();
        }
        Shared {
            known,
            identities,
            nodes,
        }
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn admin_observers_attribute_a_shared_node_to_neither_identity() {
        let Shared {
            known,
            identities: [a, b, c, d],
            nodes: [own, shared, handed],
        } = shared_handed_and_own_nodes();
        let admins = [&a, &b, &c, &d].map(PrivateKey::public_key);
        let mut observed = BTreeMap::new();
        observe_admins(&known, &admins, 42, &mut observed).unwrap();
        let attributed = |identity: &PrivateKey, node: &PrivateKey| {
            observed
                .get(&identity.public_key())
                .and_then(|nodes| nodes.get(&node.public_key()))
                .map(|observation| observation.unlinked)
        };
        assert_eq!(attributed(&a, &shared), None);
        assert_eq!(
            attributed(&b, &shared),
            None,
            "neither identity is reported as linking a node they share"
        );
        assert_eq!(attributed(&a, &own), Some(None));
        assert_eq!(
            attributed(&c, &handed),
            Some(Some(NOW)),
            "an unlinked identity keeps its unlink time"
        );
        assert_eq!(attributed(&d, &handed), Some(None));
    }
}
