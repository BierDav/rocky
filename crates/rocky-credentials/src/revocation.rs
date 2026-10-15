//! Revocation statements and the grow-only set a device holds of them. See
//! `docs/decisions/0049-mergeable-revocation-set.md`.
use crate::{
    CredentialChain, Error, PrivateKey, PublicKey, SignedCapability, decode_bounded, denied,
    encode, signing_bytes,
};
use p2panda_core::Signature;
use rocky_core::{AppId, Commitment};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const DAY: u64 = 24 * 60 * 60;
/// Longest a credential link may be valid. A statement outlives every
/// chain it ends by this much.
pub const MAX_CHAIN_LIFETIME: u64 = 366 * DAY;
/// Clock difference and backdating allowed beyond the lifetime.
pub const LIFETIME_SKEW: u64 = DAY;
/// Live statements a new revocation may join; `revoke` fails at the bound.
pub const MAX_LIVE_REVOCATIONS: usize = 1024;
/// Largest encoded signed statement.
pub const MAX_REVOCATION_BYTES: usize = 2048;
/// What the two signatures add to an encoded statement, at most.
pub const SIGNATURE_OVERHEAD: usize = 192;
/// Largest encoded set, a bound on what any merge accepts.
pub const MAX_REVOCATION_SET_BYTES: usize = 2 * 1024 * 1024;
/// The named refusal of a revocation beyond the live bound.
pub const REVOCATION_SET_FULL: &str = "revocation set full: 1,024 live statements";

/// One revocation: the keys and capability IDs it ends, the issuer that
/// made it and when. Its ID is its hash.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revocation {
    /// The application, under its publisher, the statement belongs to.
    pub app: AppId,
    /// The issuer's node key; its co-signature attributes, never authorizes.
    pub issuer: PublicKey,
    pub time: u64,
    pub nodes: BTreeSet<crate::PeerId>,
    pub users: BTreeSet<PublicKey>,
    pub capabilities: BTreeSet<Commitment>,
}
impl Revocation {
    pub fn id(&self) -> Result<Commitment, Error> {
        Ok(rocky_core::commitment(
            "rocky.revocation.id.v3",
            &encode(self)?,
        ))
    }
    /// The first day, counted from the Unix epoch, on which every chain
    /// this statement ends has expired.
    pub fn expiry_day(&self) -> u64 {
        self.time
            .saturating_add(MAX_CHAIN_LIFETIME + LIFETIME_SKEW)
            .div_ceil(DAY)
    }
    /// Whether the statement is still in the distributed set at `time`:
    /// the day-rounded cutoff the digest uses.
    pub fn live(&self, time: u64) -> bool {
        time / DAY < self.expiry_day()
    }
    /// Every key the statement revokes as a credential subject.
    pub fn subjects(&self) -> impl Iterator<Item = &PublicKey> {
        self.users.iter()
    }
    fn empty(&self) -> bool {
        self.nodes.is_empty() && self.users.is_empty() && self.capabilities.is_empty()
    }
}

/// A statement signed under the publisher key and co-signed by its issuer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedRevocation {
    pub statement: Revocation,
    publisher: Signature,
    issuer: Signature,
}
impl SignedRevocation {
    pub fn sign(
        statement: Revocation,
        publisher: &PrivateKey,
        issuer: &PrivateKey,
    ) -> Result<Self, Error> {
        if statement.issuer != issuer.public_key() || statement.empty() {
            return Err(denied("invalid revocation"));
        }
        let signed = Self {
            publisher: publisher.sign(&signing_bytes("rocky.revocation.v3", &statement)?),
            issuer: issuer.sign(&signing_bytes("rocky.revocation-issuer.v3", &statement)?),
            statement,
        };
        if encode(&signed)?.len() > MAX_REVOCATION_BYTES {
            return Err(denied("revocation exceeds its size limit"));
        }
        Ok(signed)
    }
    /// Checks both signatures and the application. Whether the issuer is
    /// itself revoked does not matter. Returns the statement's ID.
    pub fn verify(&self, publisher: PublicKey, app: AppId) -> Result<Commitment, Error> {
        if self.statement.app != app
            || self.statement.empty()
            || encode(self)?.len() > MAX_REVOCATION_BYTES
            || !publisher.verify(
                &signing_bytes("rocky.revocation.v3", &self.statement)?,
                &self.publisher,
            )
            || !self.statement.issuer.verify(
                &signing_bytes("rocky.revocation-issuer.v3", &self.statement)?,
                &self.issuer,
            )
        {
            return Err(denied("invalid revocation signature"));
        }
        self.statement.id()
    }
    pub fn id(&self) -> Result<Commitment, Error> {
        self.statement.id()
    }
    /// Whether this statement ends a chain through `link`.
    pub fn ends(&self, link: &SignedCapability) -> Result<bool, Error> {
        let subject = &link.capability.subject;
        Ok(self.statement.subjects().any(|key| key == subject)
            || link
                .capability
                .extended()?
                .node
                .is_some_and(|node| self.statement.nodes.contains(&node))
            || self.statement.capabilities.contains(&link.id()?))
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        encode(self)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        decode_bounded(bytes, MAX_REVOCATION_BYTES)
    }
}

/// The statements a device verified. Merging is union; nothing is ever
/// removed but statements past their expiry, which every holder drops by
/// the same day-rounded cutoff. A digest identifies the live set.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "Vec<SignedRevocation>", try_from = "Vec<SignedRevocation>")]
pub struct RevocationSet {
    statements: BTreeMap<Commitment, SignedRevocation>,
    subjects: BTreeSet<PublicKey>,
    capabilities: BTreeSet<Commitment>,
}
impl From<RevocationSet> for Vec<SignedRevocation> {
    fn from(set: RevocationSet) -> Self {
        set.statements.into_values().collect()
    }
}
impl TryFrom<Vec<SignedRevocation>> for RevocationSet {
    type Error = Error;
    fn try_from(statements: Vec<SignedRevocation>) -> Result<Self, Error> {
        let mut set = Self::default();
        for signed in statements {
            if !set.insert(signed)? {
                return Err(denied("duplicate revocation"));
            }
        }
        Ok(set)
    }
}
impl RevocationSet {
    pub fn new() -> Self {
        Self::default()
    }
    fn insert(&mut self, signed: SignedRevocation) -> Result<bool, Error> {
        let id = signed.id()?;
        if self.statements.contains_key(&id) {
            return Ok(false);
        }
        self.subjects.extend(signed.statement.subjects().copied());
        self.capabilities
            .extend(signed.statement.capabilities.iter().copied());
        self.statements.insert(id, signed);
        Ok(true)
    }
    /// Verifies every statement against the publisher and application.
    pub fn verify(&self, publisher: PublicKey, app: AppId) -> Result<(), Error> {
        for signed in self.statements.values() {
            signed.verify(publisher, app)?;
        }
        self.check_size()
    }
    fn check_size(&self) -> Result<(), Error> {
        if encode(self)?.len() > MAX_REVOCATION_SET_BYTES {
            return Err(denied("revocation set exceeds its size limit"));
        }
        Ok(())
    }
    /// Adds the live statements of `other` this set lacks and drops its
    /// own expired ones. A statement that fails verification, or a union
    /// over the size limit, is refused and changes nothing. Returns the
    /// statements added.
    pub fn merge(
        &mut self,
        other: &RevocationSet,
        publisher: PublicKey,
        app: AppId,
        time: u64,
    ) -> Result<Vec<SignedRevocation>, Error> {
        let mut next = self.clone();
        let mut added = Vec::new();
        for (id, signed) in &other.statements {
            if self.statements.contains_key(id) {
                continue;
            }
            signed.verify(publisher, app)?;
            if signed.statement.live(time) && next.insert(signed.clone())? {
                added.push(signed.clone());
            }
        }
        next = next.pruned(time);
        next.check_size()?;
        *self = next;
        Ok(added)
    }
    /// This set without the statements expired at `time`.
    pub fn pruned(&self, time: u64) -> Self {
        let mut set = Self::default();
        for signed in self.statements.values() {
            if signed.statement.live(time) {
                // Every held statement was inserted once already.
                let _ = set.insert(signed.clone());
            }
        }
        set
    }
    /// The statements in ID order.
    pub fn statements(&self) -> impl Iterator<Item = &SignedRevocation> {
        self.statements.values()
    }
    pub fn get(&self, id: &Commitment) -> Option<&SignedRevocation> {
        self.statements.get(id)
    }
    pub fn len(&self) -> usize {
        self.statements.len()
    }
    pub fn is_empty(&self) -> bool {
        self.statements.is_empty()
    }
    /// The statements still live at `time`.
    pub fn live(&self, time: u64) -> usize {
        self.statements
            .values()
            .filter(|signed| signed.statement.live(time))
            .count()
    }
    /// Identifies the live set at `time`: two holders of the same live
    /// statements have the same digest, whatever order they learned them.
    pub fn digest(&self, time: u64) -> Result<Commitment, Error> {
        let live: Vec<&Commitment> = self
            .statements
            .iter()
            .filter(|(_, signed)| signed.statement.live(time))
            .map(|(id, _)| id)
            .collect();
        Ok(rocky_core::commitment(
            "rocky.revocation-set.v1",
            &encode(&live)?,
        ))
    }
    /// Whether some statement revokes `key` as a credential subject.
    pub fn revokes_reference(&self, node: &crate::PeerId) -> bool {
        self.statements()
            .any(|signed| signed.statement.nodes.contains(node))
    }
    pub fn revokes(&self, key: &PublicKey) -> bool {
        self.subjects.contains(key) || self.revokes_reference(&crate::PeerId::of(*key))
    }
    /// Whether some statement ends a chain through `link`.
    pub fn ends(&self, link: &SignedCapability) -> Result<bool, Error> {
        Ok(self.subjects.contains(&link.capability.subject)
            || link
                .capability
                .extended()?
                .node
                .is_some_and(|node| self.revokes_reference(&node))
            || self.capabilities.contains(&link.id()?))
    }
    /// The first statement that ends `chain`, if any.
    pub fn revoking(&self, chain: &CredentialChain) -> Result<Option<&SignedRevocation>, Error> {
        for signed in self.statements.values() {
            for link in &chain.0 {
                if signed.ends(link)? {
                    return Ok(Some(signed));
                }
            }
        }
        Ok(None)
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        encode(self)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        decode_bounded(bytes, MAX_REVOCATION_SET_BYTES)
    }
}

#[cfg(test)]
mod tests;
