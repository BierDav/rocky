//! Verified, currently linked nodes, separate from issuer discovery records.
#[cfg(test)]
use rocky_core::PrincipalKind;
#[cfg(test)]
use rocky_credentials::CredentialChain;
use rocky_credentials::PublicKey;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Nodes an issuer remembers having admitted or renewed.
pub const MAX_SEEN: usize = crate::registry::MAX_IDENTITIES * 32;

/// A node an identity certified.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkedNode {
    pub node: PublicKey,
    /// Unknown until a certificate of it is held.
    pub author: Option<u32>,
    /// The earliest certificate or admission known of it, in epoch seconds.
    pub linked: u64,
}
/// A node an issuer admitted or renewed for `identity`, at `linked`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Seen {
    pub identity: PublicKey,
    pub linked: u64,
}
/// Remembers `node` of `identity`; the oldest goes at the bound.
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

/// Linked nodes by identity.
#[derive(Clone, Debug, Default)]
pub struct Membership(BTreeMap<PublicKey, BTreeMap<PublicKey, LinkedNode>>);
impl Membership {
    /// The view of `chains`; chains that end at no node certificate are
    /// skipped.
    #[cfg(test)]
    pub fn of<'a>(
        chains: impl IntoIterator<Item = &'a CredentialChain>,
        seen: &BTreeMap<PublicKey, Seen>,
    ) -> Self {
        let mut membership = Self::default();
        for (node, seen) in seen {
            membership.0.entry(seen.identity).or_default().insert(
                *node,
                LinkedNode {
                    node: *node,
                    author: None,
                    linked: seen.linked,
                },
            );
        }
        for chain in chains {
            let Some(leaf) = chain.0.last() else {
                continue;
            };
            let Ok(author) = chain.author_number() else {
                continue;
            };
            if leaf.capability.kind != PrincipalKind::Node {
                continue;
            }
            let node = leaf.capability.subject;
            let linked = leaf.capability.not_before;
            membership
                .0
                .entry(leaf.issuer)
                .or_default()
                .entry(node)
                .and_modify(|held| {
                    held.author = Some(author);
                    held.linked = held.linked.min(linked);
                })
                .or_insert(LinkedNode {
                    node,
                    author: Some(author),
                    linked,
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
            if set.ends_node_of(certificate.issuer, certificate.capability.subject, false) {
                continue;
            }
            let author = certificate.capability.authors()?.map(|range| range.first);
            let node = certificate.capability.subject;
            view.0.entry(certificate.issuer).or_default().insert(
                node,
                LinkedNode {
                    node,
                    author,
                    linked: certificate.capability.not_before,
                },
            );
        }
        Ok(view)
    }
    /// The nodes `identity` linked, by node key.
    pub fn nodes(&self, identity: &PublicKey) -> impl Iterator<Item = &LinkedNode> {
        self.0.get(identity).into_iter().flat_map(BTreeMap::values)
    }
    /// The identity that linked `node`.
    pub fn identity_of(&self, node: &PublicKey) -> Option<PublicKey> {
        self.0
            .iter()
            .find(|(_, nodes)| nodes.contains_key(node))
            .map(|(identity, _)| *identity)
    }
    /// Every node, with the identity that linked it.
    pub fn all(&self) -> impl Iterator<Item = (PublicKey, &LinkedNode)> {
        self.0
            .iter()
            .flat_map(|(identity, nodes)| nodes.values().map(|node| (*identity, node)))
    }
}
