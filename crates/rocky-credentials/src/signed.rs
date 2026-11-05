//! Signed manifests, capabilities and credential chains, and the
//! endorsement by which the owner or an issuer signs a domain statement.
use crate::{
    AuthorRange, Error, Extended, Extension, IdentityProvider, LIFETIME_SKEW, MAX_CHAIN_LIFETIME,
    MAX_CREDENTIAL_BYTES, MAX_CREDENTIAL_DEPTH, MAX_REVOCATION_SET_BYTES, PrivateKey, PublicKey,
    Purpose, RevocationSet, decode_bounded, denied, encode, signing_bytes,
};
use p2panda_core::Signature;
use rocky_core::{AppId, ApplicationManifest, Commitment, PrincipalKind, ResourceGrant};
use serde::{Deserialize, Serialize};

/// Most bytes an issuer chain and its node certificate add to an endorsed
/// statement.
pub const MAX_ENDORSEMENT_BYTES: usize = 16 * 1024;

/// A node certificate as its node presents it beside what it signs: the
/// capability its identity signed for the node key. It carries no grants
/// and never expires; its identity's capability decides what it may do.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Certificate {
    pub capability: Capability,
    signature: Signature,
}
impl Certificate {
    /// The certificate a chain's node leaf is.
    pub fn of(link: &SignedCapability) -> Result<Self, Error> {
        if link.attestation.certificate.is_some() || link.capability.kind != PrincipalKind::Node {
            return Err(denied("not a node certificate"));
        }
        Ok(Self {
            capability: link.capability.clone(),
            signature: link.attestation.signature,
        })
    }
    /// The certified node key, once `identity` is found to have signed this
    /// as a node certificate.
    pub fn node_of(&self, identity: PublicKey) -> Result<PublicKey, Error> {
        node_shape(&self.capability)?;
        if !identity.verify(
            &signing_bytes(CAPABILITY_LABEL, &self.capability)?,
            &self.signature,
        ) {
            return Err(denied("invalid node certificate"));
        }
        Ok(self.capability.subject)
    }
    /// The node key, as named, unverified.
    pub fn node(&self) -> PublicKey {
        self.capability.subject
    }
}
/// What every node certificate is: one author number, its own node
/// reference, no grants, no authority and no expiry.
fn node_shape(capability: &Capability) -> Result<(), Error> {
    let extended = capability.extended()?;
    if capability.kind != PrincipalKind::Node
        || capability.delegations != 0
        || !capability.grants.is_empty()
        || capability.expires != crate::NEVER
        || !extended.partitions.is_empty()
        || extended.node != Some(crate::PeerId::of(capability.subject))
        || !extended
            .authors
            .is_some_and(|range| range.single().is_some())
        || extended.issues
        || extended.trusted.is_some()
        || extended.claims.is_some()
        || extended.origin.is_some()
    {
        return Err(denied("invalid node certificate"));
    }
    Ok(())
}

/// Who signs for a principal: its own key (the root, or an identity
/// certifying its nodes), or one of its nodes under the certificate the
/// principal signed for it, which needs no principal key.
#[derive(Clone)]
pub enum Signer<'a> {
    Key(&'a dyn IdentityProvider),
    Node {
        key: &'a PrivateKey,
        identity: PublicKey,
        certificate: Certificate,
    },
}
impl<'a, T: IdentityProvider> From<&'a T> for Signer<'a> {
    fn from(key: &'a T) -> Self {
        Self::Key(key)
    }
}
impl<'a> From<&'a dyn IdentityProvider> for Signer<'a> {
    fn from(key: &'a dyn IdentityProvider) -> Self {
        Self::Key(key)
    }
}
impl<'a> Signer<'a> {
    /// The node `key` signing for its identity under `chain`, whose leaf is
    /// its certificate.
    pub fn node(key: &'a PrivateKey, chain: &CredentialChain) -> Result<Self, Error> {
        let leaf = chain
            .0
            .last()
            .ok_or_else(|| denied("missing node certificate"))?;
        let certificate = Certificate::of(leaf)?;
        if certificate.node_of(leaf.issuer)? != key.public_key() {
            return Err(denied("the certificate names another node"));
        }
        Ok(Self::Node {
            key,
            identity: leaf.issuer,
            certificate,
        })
    }
    /// The principal it signs for.
    pub fn principal(&self) -> Result<PublicKey, Error> {
        match self {
            Self::Key(key) => key.public_key(),
            Self::Node { identity, .. } => Ok(*identity),
        }
    }
    /// The signing node, when a node signs.
    pub fn signing_node(&self) -> Option<PublicKey> {
        match self {
            Self::Key(_) => None,
            Self::Node { key, .. } => Some(key.public_key()),
        }
    }
    pub(crate) fn attest(&self, purpose: Purpose, message: &[u8]) -> Result<Attestation, Error> {
        Ok(match self {
            Self::Key(key) => Attestation {
                certificate: None,
                signature: key.sign(purpose, message)?,
            },
            Self::Node {
                key, certificate, ..
            } => Attestation {
                certificate: Some(certificate.clone()),
                signature: key.sign(message),
            },
        })
    }
}

/// A principal's signature: by its own key, or by a node it certified,
/// whose certificate it carries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attestation {
    certificate: Option<Certificate>,
    signature: Signature,
}
impl Attestation {
    /// Checks that `principal`, or a node it certified, signed `message`;
    /// returns that node.
    pub(crate) fn verify(
        &self,
        principal: PublicKey,
        message: &[u8],
    ) -> Result<Option<PublicKey>, Error> {
        let node = match &self.certificate {
            None => None,
            Some(certificate) => Some(certificate.node_of(principal)?),
        };
        if !node.unwrap_or(principal).verify(message, &self.signature) {
            return Err(denied("invalid signature"));
        }
        Ok(node)
    }
    /// Whether `revocations` ends the signing node at `time`: a certificate
    /// not yet valid, or one a statement names; a `protected` one only the
    /// root's.
    pub(crate) fn node_ended(
        &self,
        (revocations, time): (&RevocationSet, u64),
        protected: bool,
    ) -> bool {
        self.certificate.as_ref().is_some_and(|certificate| {
            certificate.capability.not_before > time
                || revocations.ends_node(certificate.node(), protected)
        })
    }
    pub fn certificate(&self) -> Option<&Certificate> {
        self.certificate.as_ref()
    }
}

/// A domain statement's signature: by the owner, whose key is the
/// publisher key, or by a node of an issuer identity, carrying the issuer
/// links from the publisher and the node's certificate. Nobody else
/// endorses.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endorsement {
    /// The issuer links from the publisher to the signing identity; empty
    /// when the owner signs.
    pub chain: CredentialChain,
    attestation: Attestation,
}
impl Endorsement {
    /// `signer` signs `value` under `label`: the owner by its key with an
    /// empty `chain`, an issuer by its node with `chain` ending at its
    /// identity.
    pub fn sign<'s, T: Serialize>(
        signer: impl Into<Signer<'s>>,
        chain: &CredentialChain,
        label: &str,
        value: &T,
    ) -> Result<Self, Error> {
        let signer = signer.into();
        let principal = signer.principal()?;
        match (chain.0.last(), signer.signing_node()) {
            (None, None) => {}
            (Some(last), Some(_)) if last.capability.subject == principal => {}
            (Some(_), None) => return Err(denied("an issuer signs through its node")),
            _ => return Err(denied("the issuer chain ends at another identity")),
        }
        Ok(Self {
            chain: chain.clone(),
            attestation: signer.attest(Purpose::Statement, &signing_bytes(label, value)?)?,
        })
    }
    /// Checks that the owner or an issuer of `publisher` signed `value`,
    /// whatever the time and later revocations, and returns the signer.
    pub fn verify<T: Serialize>(
        &self,
        publisher: PublicKey,
        label: &str,
        value: &T,
    ) -> Result<PublicKey, Error> {
        self.check(publisher, label, value, None)
    }
    /// As `verify`, with the signer's chain valid at `time` and its links
    /// and node ended by no statement of `revocations`: what a newly met
    /// statement needs. A revoked issuer signs nothing that verifies here.
    pub fn verify_current<T: Serialize>(
        &self,
        publisher: PublicKey,
        label: &str,
        value: &T,
        revocations: &RevocationSet,
        time: u64,
    ) -> Result<PublicKey, Error> {
        self.check(publisher, label, value, Some((revocations, time)))
    }
    fn check<T: Serialize>(
        &self,
        publisher: PublicKey,
        label: &str,
        value: &T,
        current: Option<(&RevocationSet, u64)>,
    ) -> Result<PublicKey, Error> {
        let signer = if self.chain.0.is_empty() {
            publisher
        } else {
            self.chain.issuer(publisher, current)?
        };
        self.attested(signer, &signing_bytes(label, value)?, current)
            .map_err(|_| denied("invalid endorsement signature"))?;
        Ok(signer)
    }
    /// Checks the signature by `signer`: the owner's key, anyone else's
    /// node, unrevoked under `current`. Returns the node.
    fn attested(
        &self,
        signer: PublicKey,
        message: &[u8],
        current: Option<(&RevocationSet, u64)>,
    ) -> Result<Option<PublicKey>, Error> {
        if self.chain.0.is_empty() != self.attestation.certificate.is_none() {
            return Err(denied("only the owner signs with its own key"));
        }
        let node = self.attestation.verify(signer, message)?;
        // A trusted identity's node only the root revokes.
        let protected = self
            .chain
            .trusted_subject()
            .is_ok_and(|trusted| trusted == Some(signer));
        if current.is_some_and(|current| self.attestation.node_ended(current, protected)) {
            return Err(denied("the signing node is revoked"));
        }
        Ok(node)
    }
    /// Verifies a tip statement; returns whether a trusted identity signed
    /// it and the signing node, its lane.
    pub(crate) fn verify_tip_statement<T: Serialize>(
        &self,
        publisher: PublicKey,
        scope: rocky_core::Scope,
        label: &str,
        value: &T,
        current: Option<(&RevocationSet, u64)>,
    ) -> Result<(bool, Option<PublicKey>), Error> {
        let (signer, trusted) = if self.chain.0.is_empty() {
            (publisher, true)
        } else {
            let leaf = self.chain.links(publisher, current)?;
            if leaf.kind != PrincipalKind::User {
                return Err(denied("tip statements require an identity"));
            }
            let trusted = self.chain.0.len() == 1 && leaf.extended()?.trusted == Some(scope);
            // A trusted grant's issuer authority holds in its own domain only.
            if !trusted
                && self.chain.0.iter().any(|link| {
                    !link
                        .capability
                        .extended()
                        .is_ok_and(|e| e.issues && e.trusted.is_none_or(|held| held == scope))
                })
            {
                return Err(denied("the chain ends at no issuer"));
            }
            (leaf.subject, trusted)
        };
        let node = self
            .attested(signer, &signing_bytes(label, value)?, current)
            .map_err(|_| denied("invalid tip statement signature"))?;
        Ok((trusted, node))
    }
    /// The identity at the end of a non-empty chain: who signed through it.
    pub fn signer_of(chain: &CredentialChain) -> Option<PublicKey> {
        chain.0.last().map(|last| last.capability.subject)
    }
    /// The signing identity, as the endorsement names it, unverified.
    pub fn signer(&self, publisher: PublicKey) -> PublicKey {
        self.chain
            .0
            .last()
            .map_or(publisher, |last| last.capability.subject)
    }
    /// The signing node, as named, unverified; none when the owner signs.
    pub fn node(&self) -> Option<PublicKey> {
        self.attestation.certificate.as_ref().map(Certificate::node)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignedManifest {
    body: Vec<u8>,
    endorsement: Endorsement,
}
const MANIFEST: &str = "rocky.signed-manifest.v2";
impl SignedManifest {
    /// The owner signs.
    pub fn sign(
        manifest: &ApplicationManifest,
        owner: &dyn IdentityProvider,
    ) -> Result<Self, Error> {
        Self::endorse(manifest, owner, &CredentialChain(Vec::new()))
    }
    /// The owner, or an issuer's node through `chain`, signs.
    pub fn endorse<'s>(
        manifest: &ApplicationManifest,
        signer: impl Into<Signer<'s>>,
        chain: &CredentialChain,
    ) -> Result<Self, Error> {
        manifest.validate()?;
        let body = manifest.canonical_body();
        let endorsement = Endorsement::sign(signer, chain, MANIFEST, &body)?;
        let signed = Self { body, endorsement };
        let publisher = PublicKey::from_bytes(&manifest.publisher)
            .map_err(|_| denied("invalid manifest publisher"))?;
        signed.verify(publisher)?;
        Ok(signed)
    }
    /// An authentic manifest of `trusted_publisher`, signed by its owner or
    /// by an issuer whatever became of it later.
    pub fn verify(&self, trusted_publisher: PublicKey) -> Result<ApplicationManifest, Error> {
        let manifest = ApplicationManifest::from_body(&self.body)?;
        if manifest.publisher != *trusted_publisher.as_bytes() {
            return Err(denied("invalid manifest signature"));
        }
        self.endorsement
            .verify(trusted_publisher, MANIFEST, &self.body)
            .map_err(|_| denied("invalid manifest signature"))?;
        Ok(manifest)
    }
    /// As `verify`, by a signer no statement of `revocations` ended: what
    /// adopting a newly met manifest needs.
    pub fn verify_current(
        &self,
        trusted_publisher: PublicKey,
        revocations: &RevocationSet,
        time: u64,
    ) -> Result<ApplicationManifest, Error> {
        let manifest = ApplicationManifest::from_body(&self.body)?;
        if manifest.publisher != *trusted_publisher.as_bytes() {
            return Err(denied("invalid manifest signature"));
        }
        self.endorsement.verify_current(
            trusted_publisher,
            MANIFEST,
            &self.body,
            revocations,
            time,
        )?;
        Ok(manifest)
    }
    pub fn endorsement(&self) -> &Endorsement {
        &self.endorsement
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        encode(self)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        decode_bounded(
            bytes,
            rocky_core::MAX_MANIFEST_BYTES * 2 + 1024 + MAX_ENDORSEMENT_BYTES,
        )
    }
}

/// One link of a chain. It names no generation: grants name tables and
/// fields by name, so a chain holds in every generation of its domain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    pub subject: PublicKey,
    pub kind: PrincipalKind,
    pub grants: Vec<ResourceGrant>,
    pub not_before: u64,
    pub expires: u64,
    pub delegations: u8,
    /// Author ranges, node numbers and partition selectors.
    pub extension: Extension,
}
impl Capability {
    /// The versioned authority fields of this capability.
    pub fn extended(&self) -> Result<Extended, Error> {
        self.extension.read()
    }
    /// The author numbers this link assigns, or a node holds.
    pub fn authors(&self) -> Result<Option<AuthorRange>, Error> {
        Ok(self.extended()?.authors)
    }
}
/// The signing label of the current capability format; others are refused.
const CAPABILITY_LABEL: &str = "rocky.capability.v3";
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedCapability {
    /// Portable assignment provenance; carries no partition tuples.
    pub author_binding: crate::AuthorBinding,
    /// The principal that grants it: the publisher or the parent link's
    /// subject.
    pub issuer: PublicKey,
    pub capability: Capability,
    attestation: Attestation,
}
impl SignedCapability {
    /// `signer` signs: a node certificate, which only an identity's own key
    /// signs, when `capability` names a node.
    pub fn sign<'s>(capability: Capability, signer: impl Into<Signer<'s>>) -> Result<Self, Error> {
        let signer = signer.into();
        let purpose = match capability.kind {
            PrincipalKind::Node if signer.signing_node().is_some() => {
                return Err(denied("only an identity's own key certifies its node"));
            }
            PrincipalKind::Node => Purpose::NodeCertificate,
            _ => Purpose::Capability,
        };
        if capability.kind == PrincipalKind::Node {
            node_shape(&capability)?;
        }
        Ok(Self {
            author_binding: crate::AuthorBinding::sign(&capability, &signer)?,
            issuer: signer.principal()?,
            attestation: signer.attest(purpose, &signing_bytes(CAPABILITY_LABEL, &capability)?)?,
            capability,
        })
    }
    pub fn id(&self) -> Result<Commitment, Error> {
        Ok(rocky_core::commitment(
            "rocky.capability.id.v3",
            &encode(self)?,
        ))
    }
    /// The issuer's signature, by its key or one of its nodes.
    pub fn attestation(&self) -> &Attestation {
        &self.attestation
    }
    /// Checks the signature; returns the node that signed for the issuer.
    fn verify_signature(&self) -> Result<Option<PublicKey>, Error> {
        if self.capability.kind == PrincipalKind::Node && self.attestation.certificate.is_some() {
            return Err(denied("only an identity's own key certifies its node"));
        }
        self.attestation.verify(
            self.issuer,
            &signing_bytes(CAPABILITY_LABEL, &self.capability)?,
        )
    }
    /// The identity whose certificate this is for the node key `node`,
    /// valid from before `time`: what a coordinator checks without the
    /// rest of the chain. Revocation and the identity's capability are the
    /// domain's to enforce.
    pub fn certifies(&self, node: PublicKey, time: u64) -> Result<PublicKey, Error> {
        if Certificate::of(self)
            .and_then(|certificate| certificate.node_of(self.issuer))
            .ok()
            != Some(node)
            || self.capability.not_before > time
        {
            return Err(denied("not a current certificate of this node"));
        }
        Ok(self.issuer)
    }
}

/// Links from the publisher to a node: issuer identities, the identity
/// that holds the grants and the node certificate it signed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialChain(pub Vec<SignedCapability>);
/// A chain checked under one revocation set. Callers must discard it when that
/// set changes and check the validity window at every use.
#[derive(Clone, Debug)]
pub struct VerifiedChain {
    pub(crate) leaf: Capability,
    /// The identity link the leaf's node certificate names: what it holds.
    pub(crate) held: Capability,
    not_before: u64,
    expires: u64,
}
impl VerifiedChain {
    pub fn selectors(
        &self,
        subject: PublicKey,
        time: u64,
    ) -> Result<(Vec<ResourceGrant>, Vec<crate::PartitionGrant>), Error> {
        let leaf = &self.leaf;
        if time < self.not_before
            || time >= self.expires
            || leaf.kind != PrincipalKind::Node
            || leaf.subject != subject
        {
            return Err(denied("invalid or expired node credential"));
        }
        Ok((self.held.grants.clone(), self.held.extended()?.partitions))
    }
    pub fn author(
        &self,
        subject: PublicKey,
        author: u32,
        table: Commitment,
        time: u64,
    ) -> Result<Vec<ResourceGrant>, Error> {
        let (grants, partitions) = self.selectors(subject, time)?;
        if self.leaf.authors()?.and_then(|range| range.single()) != Some(author)
            || partitions.iter().any(|grant| grant.table == table)
        {
            return Err(denied("author number mismatch or partition tuple required"));
        }
        let grants: Vec<_> = grants
            .into_iter()
            .filter(|grant| grant.scope.table == table)
            .collect();
        if grants.is_empty() {
            return Err(denied("node has no grants for this table"));
        }
        Ok(grants)
    }
}
impl CredentialChain {
    /// Verifies signatures, attenuation and revocations once. The result is
    /// reusable only under exactly this publisher and revocation set.
    pub fn verified(
        &self,
        publisher: PublicKey,
        revocations: &RevocationSet,
        time: u64,
    ) -> Result<VerifiedChain, Error> {
        let (leaf, held) = self.node(publisher, Some((revocations, time)))?;
        Ok(VerifiedChain {
            leaf: leaf.clone(),
            held: held.clone(),
            not_before: self
                .0
                .iter()
                .map(|link| link.capability.not_before)
                .max()
                .unwrap_or(u64::MAX),
            expires: self.expires(),
        })
    }

    /// Concrete grants of a node under `revocations` at `time`.
    /// Partition leaves require `verify_partition` with a concrete tuple.
    pub fn verify(
        &self,
        publisher: PublicKey,
        subject: PublicKey,
        revocations: &RevocationSet,
        time: u64,
    ) -> Result<Vec<ResourceGrant>, Error> {
        self.check(publisher, subject, Some((revocations, time)))
    }
    /// The identity this chain grants trust for `scope`: it must be exactly
    /// one owner-direct trusted grant, valid at `time` and unrevoked.
    pub fn trusted_identity(
        &self,
        publisher: PublicKey,
        scope: rocky_core::Scope,
        revocations: &RevocationSet,
        time: u64,
    ) -> Result<PublicKey, Error> {
        let leaf = self.links(publisher, Some((revocations, time)))?;
        if self.0.len() != 1
            || leaf.kind != PrincipalKind::User
            || leaf.extended()?.trusted != Some(scope)
        {
            return Err(denied("not an owner-direct trusted grant for this domain"));
        }
        Ok(leaf.subject)
    }
    /// Whether the publisher once delegated to the node `subject` through
    /// this chain, whatever the time and later revocations. It proves who a
    /// renewal is for; it authorizes nothing. Returns the identity's link:
    /// the one its issuer granted, below any links it signed itself.
    pub fn provenance(
        &self,
        publisher: PublicKey,
        subject: PublicKey,
    ) -> Result<&Capability, Error> {
        self.check(publisher, subject, None)?;
        self.identity_index()
            .map(|index| &self.0[index].capability)
            .ok_or_else(|| denied("credential depth"))
    }
    fn check(
        &self,
        publisher: PublicKey,
        subject: PublicKey,
        current: Option<(&RevocationSet, u64)>,
    ) -> Result<Vec<ResourceGrant>, Error> {
        let (leaf, held) = self.node(publisher, current)?;
        if leaf.subject != subject {
            return Err(denied("operation/session requires a node credential"));
        }
        if current.is_some() && !held.extended()?.partitions.is_empty() {
            return Err(denied(
                "partition credentials require a concrete partition tuple",
            ));
        }
        Ok(held.grants.clone())
    }
    /// Checks every link of a chain that ends at a node certificate and
    /// returns that certificate and the identity link it names, whose
    /// grants and partitions the node holds.
    pub(crate) fn node(
        &self,
        publisher: PublicKey,
        current: Option<(&RevocationSet, u64)>,
    ) -> Result<(&Capability, &Capability), Error> {
        let leaf = self.links(publisher, current)?;
        if leaf.kind != PrincipalKind::Node {
            return Err(denied("operation/session requires a node credential"));
        }
        let held = self
            .0
            .iter()
            .rev()
            .nth(1)
            .map(|link| &link.capability)
            .ok_or_else(|| denied("credential depth"))?;
        Ok((leaf, held))
    }
    /// Checks a node's alias-bound grants without requiring its public node key.
    pub fn verify_reference_selectors(
        &self,
        publisher: PublicKey,
        node: crate::PeerId,
        revocations: &RevocationSet,
        time: u64,
    ) -> Result<(Vec<ResourceGrant>, Vec<crate::PartitionGrant>), Error> {
        let (leaf, held) = self.node(publisher, Some((revocations, time)))?;
        if leaf.extended()?.node != Some(node) {
            return Err(denied("credential does not bind the node reference"));
        }
        Ok((held.grants.clone(), held.extended()?.partitions))
    }
    /// The issuer identity a chain of issuer links ends at: every link is
    /// signed by its parent's subject, the first by the publisher, and
    /// carries issuer authority. With `current`, each is valid at its time
    /// and ended by no statement of its set.
    pub fn issuer(
        &self,
        publisher: PublicKey,
        current: Option<(&RevocationSet, u64)>,
    ) -> Result<PublicKey, Error> {
        if self.0.is_empty() {
            return Err(denied("an issuer chain needs a link"));
        }
        let leaf = self.links(publisher, current)?;
        if self
            .0
            .iter()
            .any(|link| !link.capability.extended().is_ok_and(|e| e.issues))
        {
            return Err(denied("the chain ends at no issuer"));
        }
        Ok(leaf.subject)
    }
    /// Checks every link and returns the leaf: each is signed by its
    /// parent's subject or a node it certified, the first by the publisher,
    /// narrows it in grants, validity, depth and author range, lives at
    /// most `MAX_CHAIN_LIFETIME` and, with `current`, is valid at its time
    /// and ended by no statement of its set, nor is the node that signed it.
    /// A link with issuer authority may delegate any grant and author
    /// range. A node certificate narrows nothing but the author range: it
    /// never expires, holds no grants and lasts while its identity's link
    /// does.
    pub(crate) fn links(
        &self,
        publisher: PublicKey,
        current: Option<(&RevocationSet, u64)>,
    ) -> Result<&Capability, Error> {
        if self.0.is_empty() || self.0.len() > MAX_CREDENTIAL_DEPTH {
            return Err(denied("credential depth"));
        }
        let mut issuer = publisher;
        let mut parent: Option<(&Capability, Extended)> = None;
        for (index, signed) in self.0.iter().enumerate() {
            let capability = &signed.capability;
            let extended = capability.extended()?;
            let node = capability.kind == PrincipalKind::Node;
            let stale = match current {
                Some((revocations, time)) => {
                    capability.not_before > time
                        || capability.expires <= time
                        || revocations.ends_at(signed, self.protected(index)?)?
                        || signed
                            .attestation
                            .node_ended((revocations, time), self.protected_signer(index)?)
                }
                None => false,
            };
            // Only the root's trusted grant, and its identity's own links
            // and node certificates, live until the root revokes them.
            let forever = match &parent {
                None => extended.trusted.is_some() && extended.issues,
                Some((parent, _)) => parent.expires == crate::NEVER && self.protected(index)?,
            };
            let lifetime = if node || (capability.expires == crate::NEVER && forever) {
                0
            } else {
                capability.expires - capability.not_before
            };
            let held = capability.grants.len() + extended.partitions.len();
            if signed.issuer != issuer
                || stale
                || capability.not_before >= capability.expires
                || lifetime > MAX_CHAIN_LIFETIME + LIFETIME_SKEW
                || capability.delegations as usize >= MAX_CREDENTIAL_DEPTH
                || (held == 0
                    && !node
                    && !extended.issues
                    && extended.trusted.is_none()
                    && extended.claims.is_none())
                || held > 64
                || (extended.claims.is_some()
                    && (held != 0
                        || extended.authors.is_some()
                        || extended.issues
                        || extended.trusted.is_some()
                        || capability.expires - capability.not_before
                            > crate::CLAIM_LIFETIME + crate::CLAIM_SKEW))
                || signed.verify_signature().is_err()
            {
                return Err(denied("invalid or revoked credential"));
            }
            if capability.grants.iter().any(|grant| grant.fields.named() > 256 || matches!(&grant.rows, rocky_core::RowSelection::Only(rows) if rows.is_empty() || rows.len() > 256)) {
                return Err(denied("credential resource quota"));
            }
            Self::shape(capability, &extended)?;
            if let Some((parent, parent_extended)) = &parent {
                // Issuer authority grants anything; other links narrow.
                let authority = parent_extended.issues;
                if parent.kind == PrincipalKind::Node
                    || parent.delegations == 0
                    || capability.delegations >= parent.delegations
                    || (!node
                        && (capability.not_before < parent.not_before
                            || capability.expires > parent.expires))
                    || (parent.kind == PrincipalKind::Deployment
                        && capability.kind == PrincipalKind::User)
                    || extended.trusted.is_some()
                    || (extended.issues && !authority)
                    || (!authority
                        && capability.grants.iter().any(|grant| {
                            !parent.grants.iter().any(|allowed| allowed.includes(grant))
                        }))
                    || (!authority
                        && extended.partitions.iter().any(|grant| {
                            !parent_extended
                                .partitions
                                .iter()
                                .any(|allowed| allowed.includes(grant))
                        }))
                    || (!authority
                        && extended.claims.as_ref().is_some_and(|right| {
                            !parent_extended
                                .claims
                                .as_ref()
                                .is_some_and(|held| held.includes(right))
                        }))
                    // A node under a claim link writes nothing under it.
                    || (!authority
                        && parent_extended.claims.is_none()
                        && extended.authors.is_some_and(|range| {
                            !parent_extended
                                .authors
                                .is_some_and(|parent| parent.includes(&range))
                        }))
                {
                    return Err(denied("delegation expands authority"));
                }
                // An origin naming the signer, or the signer's origin, has
                // that grant's time: a recovery judges the origin by it.
                let named = if parent_extended.trusted.is_some() {
                    Some((parent.subject, parent.not_before))
                } else {
                    parent_extended
                        .origin
                        .map(|held| (held.identity, held.since))
                };
                if extended.origin.is_some_and(|origin| {
                    named.is_some_and(|(identity, since)| {
                        identity == origin.identity && since != origin.since
                    })
                }) {
                    return Err(denied("an origin misstates its trusted grant"));
                }
            } else if capability.kind != PrincipalKind::User {
                return Err(denied("publisher must delegate to user"));
            }
            issuer = capability.subject;
            parent = Some((capability, extended));
        }
        parent
            .map(|(leaf, _)| leaf)
            .ok_or_else(|| denied("missing leaf credential"))
    }
    /// The trusted identity whose root-signed grant starts this chain.
    pub(crate) fn trusted_subject(&self) -> Result<Option<PublicKey>, Error> {
        let Some(first) = self.0.first() else {
            return Ok(None);
        };
        Ok(first
            .capability
            .extended()?
            .trusted
            .map(|_| first.capability.subject))
    }
    /// Whether link `index` is a trusted identity's own: its root-signed
    /// grant, a link it signed itself or one of its node certificates.
    /// Only the root's statements end those; a trusted node never revokes
    /// another.
    pub(crate) fn protected(&self, index: usize) -> Result<bool, Error> {
        if index == 0 {
            return Ok(true);
        }
        let link = &self.0[index];
        Ok(self.trusted_subject()?.is_some_and(|trusted| {
            link.issuer == trusted
                && (link.capability.subject == trusted
                    || link.capability.kind == PrincipalKind::Node)
        }))
    }
    /// Whether link `index` was signed by a node of the trusted identity
    /// starting this chain, which only the root revokes.
    fn protected_signer(&self, index: usize) -> Result<bool, Error> {
        Ok(self
            .trusted_subject()?
            .is_some_and(|trusted| self.0[index].issuer == trusted))
    }
    /// Node certificates carry exactly one number of their identity's range
    /// and nothing else (see `node_shape`). Only identities hold issuer
    /// authority.
    fn shape(capability: &Capability, extended: &Extended) -> Result<(), Error> {
        let valid = match capability.kind {
            PrincipalKind::Node => node_shape(capability).is_ok(),
            PrincipalKind::User => extended.node.is_none(),
            PrincipalKind::Deployment => {
                extended.node.is_none()
                    && !extended.issues
                    && extended.trusted.is_none()
                    && extended.claims.is_none()
            }
        };
        if valid {
            Ok(())
        } else {
            Err(denied("invalid credential extension for its principal"))
        }
    }
    /// Verifies a node's number and returns its concrete grants for this table.
    pub fn verify_author(
        &self,
        publisher: PublicKey,
        signer: PublicKey,
        author: u32,
        table: Commitment,
        revocations: &RevocationSet,
        time: u64,
    ) -> Result<Vec<ResourceGrant>, Error> {
        let (grants, partitions) = self.verify_selectors(publisher, signer, revocations, time)?;
        if self.author_number()? != author || partitions.iter().any(|grant| grant.table == table) {
            return Err(denied("author number mismatch or partition tuple required"));
        }
        let grants: Vec<_> = grants
            .into_iter()
            .filter(|grant| grant.scope.table == table)
            .collect();
        if grants.is_empty() {
            return Err(denied("node has no grants for this table"));
        }
        Ok(grants)
    }
    /// The number certified for this node. Verification checks its granting range.
    pub fn author_number(&self) -> Result<u32, Error> {
        let leaf = self
            .0
            .last()
            .ok_or_else(|| denied("missing node credential"))?;
        if leaf.capability.kind != PrincipalKind::Node {
            return Err(denied("node credential required"));
        }
        leaf.capability
            .authors()?
            .and_then(|range| range.single())
            .ok_or_else(|| denied("node requires exactly one author number"))
    }
    /// Verifies the node key bound to a credential reference.
    pub fn verify_node_alias(
        &self,
        publisher: PublicKey,
        node: crate::PeerId,
        alias: PublicKey,
    ) -> Result<(), Error> {
        let (leaf, _) = self.node(publisher, None)?;
        if leaf.subject != alias || leaf.extended()?.node != Some(node) {
            return Err(denied("credential alias does not bind the node reference"));
        }
        Ok(())
    }
    /// The identity whose node the chain certifies: the subject of the
    /// link its leaf certificate names, or of the last link without one.
    pub fn principal(&self) -> Option<PublicKey> {
        let leaf = self.0.last()?;
        match leaf.capability.kind {
            PrincipalKind::Node => Some(leaf.issuer),
            _ => Some(leaf.capability.subject),
        }
    }
    /// The earliest expiry in the chain; zero for an empty chain.
    pub fn expires(&self) -> u64 {
        self.0
            .iter()
            .map(|signed| signed.capability.expires)
            .min()
            .unwrap_or(0)
    }
    /// The node certificate this chain ends at, if any.
    pub fn certificate(&self) -> Option<&SignedCapability> {
        self.0
            .last()
            .filter(|leaf| leaf.capability.kind == PrincipalKind::Node)
    }
    /// The index of the identity's link: the one its issuer granted, below
    /// which only links the identity signed itself and its node
    /// certificate follow.
    fn identity_index(&self) -> Option<usize> {
        let identity = self.certificate()?.issuer;
        let mut index = self.0.len().checked_sub(2)?;
        if self.0[index].capability.subject != identity {
            return None;
        }
        while index > 0 && self.0[index].issuer == identity {
            index -= 1;
        }
        Some(index)
    }
    /// The identity's capability chain as its issuers granted it: without
    /// the node certificate and the links the identity signed itself. A
    /// renewal replaces this part only. A chain without a node certificate
    /// is returned whole.
    pub fn capabilities(&self) -> Self {
        match self.identity_index() {
            Some(index) => Self(self.0[..=index].to_vec()),
            None => self.delegable(),
        }
    }
    /// This chain with `certificate` as its node leaf instead of any it had.
    pub fn with_certificate(&self, certificate: SignedCapability) -> Self {
        let mut chain = self.delegable();
        chain.0.push(certificate);
        chain
    }
}

/// Everything a device presents to use a domain, from any issuer: the
/// pinned publisher, the revocations it knows, its identity's capability
/// chain and its node certificate. It names no generation; any generation
/// of the domain admits it. A renewal replaces the capabilities only.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Credentials {
    pub publisher: PublicKey,
    pub revocations: RevocationSet,
    /// The links from the publisher to the identity, with any the identity
    /// signed itself.
    pub capabilities: CredentialChain,
    /// The identity's certificate of this node; none while an issuer hands
    /// the capabilities to an identity whose node certifies itself.
    pub certificate: Option<SignedCapability>,
}
/// What verified credentials establish.
#[derive(Clone, Debug)]
pub struct Verified {
    /// The identity that holds the grants.
    pub principal: PublicKey,
    pub device: PublicKey,
    /// The digest of the live revocations verified against.
    pub revocations: Commitment,
    pub expires: u64,
    pub grants: Vec<ResourceGrant>,
}
impl Credentials {
    /// The node's whole chain: the capabilities, then any certificate.
    pub fn chain(&self) -> CredentialChain {
        let mut chain = self.capabilities.clone();
        chain.0.extend(self.certificate.clone());
        chain
    }
    /// The two parts of a node's whole `chain`.
    pub fn of(publisher: PublicKey, revocations: RevocationSet, chain: &CredentialChain) -> Self {
        Self {
            publisher,
            revocations,
            capabilities: chain.delegable(),
            certificate: chain.certificate().cloned(),
        }
    }
    /// Verifies the whole set for `device` of application `app` at `time`.
    /// Needs no database, endpoint or issuer.
    pub fn verify(&self, device: PublicKey, app: AppId, time: u64) -> Result<Verified, Error> {
        self.revocations.verify(self.publisher, app)?;
        let chain = self.chain();
        let grants = chain.verify(self.publisher, device, &self.revocations, time)?;
        if grants.iter().any(|grant| grant.scope.domain.app != app) {
            return Err(denied("credentials for another application"));
        }
        Ok(Verified {
            principal: chain
                .principal()
                .ok_or_else(|| denied("credential depth"))?,
            device,
            revocations: self.revocations.digest(time)?,
            expires: chain.expires(),
            grants,
        })
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        encode(self)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        decode_bounded(bytes, MAX_CREDENTIAL_BYTES + MAX_REVOCATION_SET_BYTES)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PrivateKey;

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn a_capability_of_the_earlier_format_is_refused_and_names_no_generation() {
        let (publisher, user, node) = (PrivateKey::new(), PrivateKey::new(), PrivateKey::new());
        let grant = ResourceGrant {
            scope: rocky_core::ResourceScope {
                domain: rocky_core::Scope {
                    mesh: rocky_core::MeshId([1; 16]),
                    app: AppId([2; 16]),
                    domain: rocky_core::DomainId([3; 16]),
                },
                encryption: [4; 32],
                table: [5; 32],
            },
            rows: rocky_core::RowSelection::All,
            rights: [rocky_core::Right::Read].into(),
            fields: rocky_core::FieldSelection::All,
        };
        let root = crate::Authority::owner(&publisher)
            .issue(
                user.public_key(),
                vec![grant],
                crate::AuthorRange::ALL,
                (100, 1000, 0),
            )
            .unwrap();
        let chain = crate::device_chain(root.clone(), &user, node.public_key(), 1).unwrap();
        let revocations = RevocationSet::new();
        assert!(
            chain
                .verify(publisher.public_key(), node.public_key(), &revocations, 200)
                .is_ok()
        );
        let mut earlier = root;
        earlier.attestation.signature =
            publisher.sign(&signing_bytes("rocky.capability.v2", &earlier.capability).unwrap());
        let earlier = crate::device_chain(earlier, &user, node.public_key(), 1).unwrap();
        assert!(
            earlier
                .verify(publisher.public_key(), node.public_key(), &revocations, 200)
                .is_err()
        );
    }
}
