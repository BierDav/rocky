//! Revocation statements and the grow-only set a device holds of them.
use crate::{
    Attestation, CredentialChain, Error, PublicKey, Purpose, SignedCapability, Signer,
    decode_bounded, denied, encode, signing_bytes,
};
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
/// Largest encoded signed statement, its signer's issuer chain included.
pub const MAX_REVOCATION_BYTES: usize = 8 * 1024;
/// What the signature and the framing add to an encoded statement and
/// its signer's chain, at most.
pub const SIGNATURE_OVERHEAD: usize = 768;
/// Largest encoded set, a bound on what any merge accepts.
pub const MAX_REVOCATION_SET_BYTES: usize = 2 * 1024 * 1024;
/// The named refusal of a revocation beyond the live bound.
pub const REVOCATION_SET_FULL: &str = "revocation set full: 1,024 live statements";

/// One revocation: the identities, node certificates and capability IDs it
/// ends, the identity that signed it and when. Its ID is its hash. Signed
/// by the root it is permanent; otherwise it leaves the set once every
/// chain it ends has expired.
/// Naming an identity ends every chain through it, so all its nodes;
/// naming a node ends only that node's certificate; naming a claim right's
/// grant ends every link of that right.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revocation {
    /// The application, under its publisher, the statement belongs to.
    pub app: AppId,
    /// The signing identity: the owner's or an issuer's.
    pub issuer: PublicKey,
    pub time: u64,
    pub nodes: BTreeSet<crate::PeerId>,
    pub identities: BTreeSet<PublicKey>,
    pub capabilities: BTreeSet<Commitment>,
    /// Only in a root statement, ordered by identity and node without
    /// repeats: for each node (lane) of a trusted identity the statement
    /// revokes, the heads of its chains that stay valid. A chain without
    /// an entry keeps nothing once its signer is revoked.
    pub cut: Vec<Cut>,
    /// Only in a root statement: the recovery that signed it.
    pub recovery: Option<RootRecovery>,
}
/// A root recovery's statement: it ends every trusted grant valid before
/// the recovered identity's own, with what those identities first
/// enrolled, whether or not the recovering store knew them. Grants the
/// root signs later stay. Ordered by time, then key: of two recoveries
/// the later ends the earlier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RootRecovery {
    /// The `not_before` of the recovered identity's trusted grant.
    pub since: u64,
    /// The recovered identity.
    pub keep: PublicKey,
    /// The recovery slot of author numbers its grant holds.
    pub slot: u32,
}
impl RootRecovery {
    /// Whether it ends a trusted grant to `identity` valid from `since`.
    pub fn ends(&self, since: u64, identity: &PublicKey) -> bool {
        (since, *identity) < (self.since, self.keep)
    }
}
/// The heads a root statement keeps of one node's chains: the tip
/// statements `identity` signed through `lane`, every branch's head in
/// order, and the node's terminal acknowledgments. Empty keeps none.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Cut {
    pub identity: PublicKey,
    pub lane: PublicKey,
    pub statements: Vec<Head>,
    pub acknowledgments: Option<Head>,
}
/// A kept chain head and its place, so admission refuses a later
/// statement without holding the head.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Head {
    pub sequence: u64,
    pub id: Commitment,
}
/// Chains a root statement may cut, and heads in all, bounded by the
/// statement's size.
pub const MAX_CUT_LANES: usize = 32;
pub const MAX_CUT_HEADS: usize = 64;
impl Revocation {
    pub fn id(&self) -> Result<Commitment, Error> {
        Ok(rocky_core::commitment(
            "rocky.revocation.id.v4",
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
        self.identities.iter()
    }
    fn empty(&self) -> bool {
        self.nodes.is_empty()
            && self.identities.is_empty()
            && self.capabilities.is_empty()
            && self.recovery.is_none()
    }
    /// A recovery only in a root statement, naming a slot that exists.
    fn valid_recovery(&self, root: bool) -> bool {
        self.recovery
            .is_none_or(|recovery| root && recovery.slot < crate::RECOVERY_SLOTS)
    }
    /// A cut only in a root statement, ordered without repeats, naming
    /// chains of identities or nodes the statement revokes; a recovery's
    /// may name any identity but the one it keeps, since it ends grants
    /// its signer never saw.
    fn valid_cut(&self, root: bool) -> bool {
        (root || self.cut.is_empty())
            && self.cut.len() <= MAX_CUT_LANES
            && self
                .cut
                .windows(2)
                .all(|pair| (pair[0].identity, pair[0].lane) < (pair[1].identity, pair[1].lane))
            && self
                .cut
                .iter()
                .map(|cut| cut.statements.len())
                .sum::<usize>()
                <= MAX_CUT_HEADS
            && self.cut.iter().all(|cut| {
                (self.identities.contains(&cut.identity)
                    || self.nodes.contains(&crate::PeerId::of(cut.lane))
                    || self
                        .recovery
                        .is_some_and(|recovery| recovery.keep != cut.identity))
                    && cut.statements.windows(2).all(|pair| pair[0] < pair[1])
            })
    }
}

/// A statement signed by the owner's key, or by a node of an issuer
/// identity whose chain it carries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedRevocation {
    pub statement: Revocation,
    /// The issuer links from the publisher to the signer; empty for the owner.
    pub chain: CredentialChain,
    attestation: Attestation,
}
const SIGNED: &str = "rocky.revocation.v6";
impl SignedRevocation {
    pub fn sign<'s>(
        statement: Revocation,
        signer: impl Into<Signer<'s>>,
        chain: &CredentialChain,
    ) -> Result<Self, Error> {
        let signer = signer.into();
        if statement.issuer != signer.principal()?
            || chain.0.is_empty() == signer.signing_node().is_some()
            || statement.empty()
            || !statement.valid_cut(chain.0.is_empty())
            || !statement.valid_recovery(chain.0.is_empty())
        {
            return Err(denied("invalid revocation"));
        }
        let signed = Self {
            attestation: signer.attest(Purpose::Revocation, &signing_bytes(SIGNED, &statement)?)?,
            chain: chain.clone(),
            statement,
        };
        if encode(&signed)?.len() > MAX_REVOCATION_BYTES {
            return Err(denied("revocation exceeds its size limit"));
        }
        Ok(signed)
    }
    /// Checks the signature, that the owner's key or a node of an issuer
    /// whose chain and certificate were valid at the statement's time
    /// signed, and the application. The owner identity is never revoked.
    /// Whether the issuer or its node is revoked later does not matter.
    /// Returns the statement's ID.
    pub fn verify(&self, publisher: PublicKey, app: AppId) -> Result<Commitment, Error> {
        let statement = &self.statement;
        let time = statement.time;
        let signer = if self.chain.0.is_empty() {
            Ok(publisher)
        } else if self
            .chain
            .0
            .iter()
            .all(|link| link.capability.not_before <= time && time < link.capability.expires)
        {
            self.chain.issuer(publisher, None)
        } else {
            Err(denied(
                "the issuer chain was not valid at the statement's time",
            ))
        };
        if statement.app != app
            || statement.empty()
            || !statement.valid_cut(self.chain.0.is_empty())
            || !statement.valid_recovery(self.chain.0.is_empty())
            || statement
                .recovery
                .is_some_and(|recovery| recovery.keep == publisher)
            || statement.identities.contains(&publisher)
            || encode(self)?.len() > MAX_REVOCATION_BYTES
            || signer.ok() != Some(statement.issuer)
            || self.chain.0.is_empty() != self.attestation.certificate().is_none()
            || self
                .attestation
                .certificate()
                .is_some_and(|certificate| certificate.capability.not_before > time)
            || self
                .attestation
                .verify(statement.issuer, &signing_bytes(SIGNED, statement)?)
                .is_err()
        {
            return Err(denied("invalid revocation signature"));
        }
        statement.id()
    }
    pub fn id(&self) -> Result<Commitment, Error> {
        self.statement.id()
    }
    /// Whether the root signed it.
    pub fn root(&self) -> bool {
        self.chain.0.is_empty()
    }
    /// Whether it is still in the distributed set at `time`. A root
    /// statement always is: it may end a trusted grant, which never expires.
    pub fn live(&self, time: u64) -> bool {
        self.root() || self.statement.live(time)
    }
    /// The first day it is no longer live; `u64::MAX` for a root statement.
    pub fn expiry_day(&self) -> u64 {
        if self.root() {
            u64::MAX
        } else {
            self.statement.expiry_day()
        }
    }
    /// Whether this statement ends a chain through `link`, a `protected`
    /// one (see `CredentialChain::protected`) only when the root signed
    /// it. A root statement naming an identity also ends every link that
    /// identity first enrolled; a recovery's, every trusted grant before
    /// its own and what those first enrolled.
    pub fn ends_at(&self, link: &SignedCapability, protected: bool) -> Result<bool, Error> {
        if protected && !self.root() {
            return Ok(false);
        }
        let subject = &link.capability.subject;
        let extended = link.capability.extended()?;
        let capabilities = &self.statement.capabilities;
        let recovery = self.statement.recovery.filter(|_| self.root());
        Ok(self.statement.subjects().any(|key| key == subject)
            || (self.root()
                && extended.origin.is_some_and(|origin| {
                    self.statement.identities.contains(&origin.identity)
                        || recovery.is_some_and(|r| r.ends(origin.since, &origin.identity))
                }))
            || (extended.trusted.is_some()
                && recovery.is_some_and(|r| r.ends(link.capability.not_before, subject)))
            || extended
                .node
                .is_some_and(|node| self.statement.nodes.contains(&node))
            || capabilities.contains(&link.id()?)
            || extended
                .claims
                .is_some_and(|right| capabilities.contains(&right.grant)))
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
    references: BTreeSet<crate::PeerId>,
    capabilities: BTreeSet<Commitment>,
    /// What root statements name, which alone end protected links.
    root: Ended,
    /// The heads root statements keep, by signing identity and lane.
    cuts: BTreeMap<(PublicKey, PublicKey), Heads>,
    /// The latest recovery: it ends every trusted grant an earlier one does.
    recovery: Option<RootRecovery>,
}
/// Every head root statements keep of one node's two chains.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Heads {
    pub statements: BTreeSet<Head>,
    pub acknowledgments: BTreeSet<Head>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Ended {
    subjects: BTreeSet<PublicKey>,
    references: BTreeSet<crate::PeerId>,
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
        self.references
            .extend(signed.statement.nodes.iter().copied());
        self.capabilities
            .extend(signed.statement.capabilities.iter().copied());
        if signed.root() {
            let root = &mut self.root;
            root.subjects.extend(signed.statement.subjects().copied());
            root.references
                .extend(signed.statement.nodes.iter().copied());
            root.capabilities
                .extend(signed.statement.capabilities.iter().copied());
            for cut in &signed.statement.cut {
                let heads = self.cuts.entry((cut.identity, cut.lane)).or_default();
                heads.statements.extend(cut.statements.iter().copied());
                heads.acknowledgments.extend(cut.acknowledgments);
            }
            self.recovery = self.recovery.max(signed.statement.recovery);
        }
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
            if signed.live(time) && next.insert(signed.clone())? {
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
            if signed.live(time) {
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
            .filter(|signed| signed.live(time))
            .count()
    }
    /// Identifies the live set at `time`: two holders of the same live
    /// statements have the same digest, whatever order they learned them.
    pub fn digest(&self, time: u64) -> Result<Commitment, Error> {
        let live: Vec<&Commitment> = self
            .statements
            .iter()
            .filter(|(_, signed)| signed.live(time))
            .map(|(id, _)| id)
            .collect();
        Ok(rocky_core::commitment(
            "rocky.revocation-set.v1",
            &encode(&live)?,
        ))
    }
    /// The heads root statements keep of what `identity` signed through
    /// `lane`, when one cuts that chain.
    pub fn cut(&self, identity: &PublicKey, lane: &PublicKey) -> Option<&Heads> {
        self.cuts.get(&(*identity, *lane))
    }
    /// Every chain a root statement cuts, by identity and lane.
    pub fn cut_lanes(&self) -> impl Iterator<Item = (&(PublicKey, PublicKey), &Heads)> {
        self.cuts.iter()
    }
    /// Whether some statement revokes `key` as a credential subject.
    pub fn revokes_reference(&self, node: &crate::PeerId) -> bool {
        self.references.contains(node)
    }
    pub fn revokes(&self, key: &PublicKey) -> bool {
        self.subjects.contains(key) || self.revokes_reference(&crate::PeerId::of(*key))
    }
    /// Whether some statement names `id` among the capabilities it ends:
    /// a link's ID or a claim right's grant.
    pub fn revokes_capability(&self, id: &Commitment) -> bool {
        self.capabilities.contains(id)
    }
    /// Whether a root statement revoked the trusted identity `origin`, and
    /// with it every identity it first enrolled.
    pub fn revokes_origin(&self, origin: &PublicKey) -> bool {
        self.root.subjects.contains(origin)
    }
    /// Whether a root statement ends what `origin` first enrolled: it
    /// revoked the identity, or a recovery ended its grant.
    pub fn ends_origin(&self, origin: &crate::Origin) -> bool {
        self.revokes_origin(&origin.identity) || self.recovery_ends(origin.since, &origin.identity)
    }
    /// Whether a root statement ends the trusted grant `grant`.
    pub fn ends_trusted(&self, grant: &crate::Capability) -> bool {
        self.revokes_origin(&grant.subject) || self.recovery_ends(grant.not_before, &grant.subject)
    }
    /// Whether a recovery ends a trusted grant to `identity` valid from
    /// `since`.
    pub fn recovery_ends(&self, since: u64, identity: &PublicKey) -> bool {
        self.recovery
            .is_some_and(|recovery| recovery.ends(since, identity))
    }
    /// The latest recovery: of the recovered identities only its own
    /// stays trusted.
    pub fn recovery(&self) -> Option<&RootRecovery> {
        self.recovery.as_ref()
    }
    /// Every recovery the root signed, oldest first.
    pub fn recoveries(&self) -> Vec<RootRecovery> {
        let mut recoveries: Vec<RootRecovery> = self
            .statements
            .values()
            .filter(|signed| signed.root())
            .filter_map(|signed| signed.statement.recovery)
            .collect();
        recoveries.sort_unstable();
        recoveries.dedup();
        recoveries
    }
    /// Whether some statement ends a chain through `link`: its subject,
    /// node, capability ID, on a claim link its grant, or a root
    /// statement its origin. A `protected` link only a root statement ends.
    pub fn ends_at(&self, link: &SignedCapability, protected: bool) -> Result<bool, Error> {
        let extended = link.capability.extended()?;
        let (subjects, references, capabilities) = if protected {
            let root = &self.root;
            (&root.subjects, &root.references, &root.capabilities)
        } else {
            (&self.subjects, &self.references, &self.capabilities)
        };
        Ok(subjects.contains(&link.capability.subject)
            || extended.node.is_some_and(|node| references.contains(&node))
            || capabilities.contains(&link.id()?)
            || extended
                .claims
                .is_some_and(|right| capabilities.contains(&right.grant))
            || (extended.trusted.is_some()
                && self.recovery_ends(link.capability.not_before, &link.capability.subject))
            || extended
                .origin
                .is_some_and(|origin| self.ends_origin(&origin)))
    }
    /// Whether some statement ends the node certificate of `node`: names
    /// the node or its key. A `protected` one only a root statement ends.
    pub fn ends_node(&self, node: PublicKey, protected: bool) -> bool {
        let (subjects, references) = if protected {
            (&self.root.subjects, &self.root.references)
        } else {
            (&self.subjects, &self.references)
        };
        subjects.contains(&node) || references.contains(&crate::PeerId::of(node))
    }
    /// The first statement that revokes the holder of `chain`: its node
    /// certificate or the identity that signed it. A statement ending an
    /// issuer above them ends the chain too, yet another issuer renews it.
    pub fn revoking_holder(
        &self,
        chain: &CredentialChain,
    ) -> Result<Option<&SignedRevocation>, Error> {
        let start = chain.0.len().saturating_sub(2);
        self.first_ending(chain, start)
    }
    /// The first statement that ends `chain`, if any.
    pub fn revoking(&self, chain: &CredentialChain) -> Result<Option<&SignedRevocation>, Error> {
        self.first_ending(chain, 0)
    }
    /// Whether some statement ends a link of `chain`, each judged by its
    /// position.
    pub fn ends_chain(&self, chain: &CredentialChain) -> Result<bool, Error> {
        for (index, link) in chain.0.iter().enumerate() {
            if self.ends_at(link, chain.protected(index)?)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
    fn first_ending(
        &self,
        chain: &CredentialChain,
        start: usize,
    ) -> Result<Option<&SignedRevocation>, Error> {
        for signed in self.statements.values() {
            for (index, link) in chain.0.iter().enumerate().skip(start) {
                if signed.ends_at(link, chain.protected(index)?)? {
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
