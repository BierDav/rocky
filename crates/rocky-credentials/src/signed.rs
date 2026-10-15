//! Signed manifests, capabilities and credential chains, and the
//! endorsement by which the owner or an issuer signs a domain statement.
use crate::{
    AuthorRange, Error, Extended, Extension, IdentityProvider, LIFETIME_SKEW, MAX_CHAIN_LIFETIME,
    MAX_CREDENTIAL_BYTES, MAX_CREDENTIAL_DEPTH, MAX_REVOCATION_SET_BYTES, PublicKey, Purpose,
    RevocationSet, decode_bounded, denied, encode, signing_bytes,
};
use p2panda_core::Signature;
use rocky_core::{AppId, ApplicationManifest, Commitment, PrincipalKind, ResourceGrant};
use serde::{Deserialize, Serialize};

/// Most bytes an issuer chain adds to an endorsed statement.
pub const MAX_ENDORSEMENT_BYTES: usize = 16 * 1024;

/// A domain statement's signature: by the owner, whose identity key is the
/// publisher key, or by an issuer identity through its chain of issuer
/// links from the publisher. Nobody else endorses.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endorsement {
    /// The issuer links from the publisher to the signer; empty when the
    /// owner signs.
    pub chain: CredentialChain,
    signature: Signature,
}
impl Endorsement {
    /// `signer` signs `value` under `label`; `chain` ends at its identity,
    /// or is empty when it is the owner's.
    pub fn sign<T: Serialize>(
        signer: &dyn IdentityProvider,
        chain: &CredentialChain,
        label: &str,
        value: &T,
    ) -> Result<Self, Error> {
        let key = signer.public_key()?;
        if chain
            .0
            .last()
            .is_some_and(|last| last.capability.subject != key)
        {
            return Err(denied("the issuer chain ends at another identity"));
        }
        Ok(Self {
            chain: chain.clone(),
            signature: signer.sign(Purpose::Statement, &signing_bytes(label, value)?)?,
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
    /// As `verify`, with the signer's chain valid at `time` and ended by no
    /// statement of `revocations`: what a newly met statement needs. A
    /// revoked issuer signs nothing that verifies here.
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
        if !signer.verify(&signing_bytes(label, value)?, &self.signature) {
            return Err(denied("invalid endorsement signature"));
        }
        Ok(signer)
    }
    pub(crate) fn verify_tip_statement<T: Serialize>(
        &self,
        publisher: PublicKey,
        scope: rocky_core::Scope,
        label: &str,
        value: &T,
        current: Option<(&RevocationSet, u64)>,
    ) -> Result<bool, Error> {
        let (signer, trusted) = if self.chain.0.is_empty() {
            (publisher, true)
        } else {
            let leaf = self.chain.links(publisher, current)?;
            if leaf.kind != PrincipalKind::User {
                return Err(denied("tip statements require an identity"));
            }
            let trusted = self.chain.0.len() == 1 && leaf.extended()?.trusted == Some(scope);
            if !trusted {
                self.chain.issuer(publisher, current)?;
            }
            (leaf.subject, trusted)
        };
        if !signer.verify(&signing_bytes(label, value)?, &self.signature) {
            return Err(denied("invalid tip statement signature"));
        }
        Ok(trusted)
    }
    /// The signing identity, as the endorsement names it, unverified.
    pub fn signer(&self, publisher: PublicKey) -> PublicKey {
        self.chain
            .0
            .last()
            .map_or(publisher, |last| last.capability.subject)
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
    /// The owner, or an issuer through `chain`, signs.
    pub fn endorse(
        manifest: &ApplicationManifest,
        signer: &dyn IdentityProvider,
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
        let manifest = self.verify(trusted_publisher)?;
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

/// A publisher's transition from one signed manifest to the next. See
/// `docs/runtime/generations.md`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedTransition {
    body: Vec<u8>,
    endorsement: Endorsement,
}
const TRANSITION: &str = "rocky.signed-transition.v2";
impl SignedTransition {
    /// The owner signs.
    pub fn sign(
        transition: &rocky_core::GenerationTransition,
        owner: &dyn IdentityProvider,
    ) -> Result<Self, Error> {
        Self::endorse(transition, owner, &CredentialChain(Vec::new()))
    }
    /// The owner, or an issuer through `chain`, signs.
    pub fn endorse(
        transition: &rocky_core::GenerationTransition,
        signer: &dyn IdentityProvider,
        chain: &CredentialChain,
    ) -> Result<Self, Error> {
        transition.validate()?;
        let body = transition.canonical();
        Ok(Self {
            endorsement: Endorsement::sign(signer, chain, TRANSITION, &body)?,
            body,
        })
    }
    pub fn verify(&self, publisher: PublicKey) -> Result<rocky_core::GenerationTransition, Error> {
        let transition = rocky_core::GenerationTransition::from_canonical(&self.body)?;
        self.endorsement
            .verify(publisher, TRANSITION, &self.body)
            .map_err(|_| denied("invalid transition signature"))?;
        Ok(transition)
    }
    /// As `verify`, by a signer no statement of `revocations` ended.
    pub fn verify_current(
        &self,
        publisher: PublicKey,
        revocations: &RevocationSet,
        time: u64,
    ) -> Result<rocky_core::GenerationTransition, Error> {
        let transition = self.verify(publisher)?;
        self.endorsement
            .verify_current(publisher, TRANSITION, &self.body, revocations, time)?;
        Ok(transition)
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        encode(self)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        decode_bounded(
            bytes,
            rocky_core::MAX_TRANSITION_IDS * 34 + 1024 + MAX_ENDORSEMENT_BYTES,
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
const CAPABILITY_LABEL: &str = "rocky.capability.v2";
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedCapability {
    /// Portable assignment provenance; carries no partition tuples.
    pub author_binding: crate::AuthorBinding,
    pub issuer: PublicKey,
    pub capability: Capability,
    signature: Signature,
}
impl SignedCapability {
    /// `issuer` signs: a node certificate when `capability` names a node.
    pub fn sign(capability: Capability, issuer: &dyn IdentityProvider) -> Result<Self, Error> {
        let purpose = match capability.kind {
            PrincipalKind::Node => Purpose::NodeCertificate,
            _ => Purpose::Capability,
        };
        let signature = issuer.sign(purpose, &signing_bytes(CAPABILITY_LABEL, &capability)?)?;
        Ok(Self {
            author_binding: crate::AuthorBinding::sign(&capability, issuer)?,
            issuer: issuer.public_key()?,
            capability,
            signature,
        })
    }
    pub fn id(&self) -> Result<Commitment, Error> {
        Ok(rocky_core::commitment(
            "rocky.capability.id.v2",
            &encode(self)?,
        ))
    }
    /// The identity whose certificate this is for the node key `node`,
    /// valid at `time`: what a coordinator checks without the rest of the
    /// chain. Revocation is the domain's to enforce.
    pub fn certifies(&self, node: PublicKey, time: u64) -> Result<PublicKey, Error> {
        let capability = &self.capability;
        if capability.kind != PrincipalKind::Node
            || capability.delegations != 0
            || capability.extended()?.node != Some(crate::PeerId::of(node))
            || capability.not_before > time
            || capability.expires <= time
            || !self.issuer.verify(
                &signing_bytes(CAPABILITY_LABEL, capability)?,
                &self.signature,
            )
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
impl CredentialChain {
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
    /// Whether the publisher once delegated to the node `subject` through
    /// this chain, whatever the time and later revocations. It proves who a
    /// renewal is for; it authorizes nothing. Returns the identity's link:
    /// the one its node certificate names.
    pub fn provenance(
        &self,
        publisher: PublicKey,
        subject: PublicKey,
    ) -> Result<&Capability, Error> {
        self.check(publisher, subject, None)?;
        self.0
            .iter()
            .rev()
            .nth(1)
            .map(|identity| &identity.capability)
            .ok_or_else(|| denied("credential depth"))
    }
    fn check(
        &self,
        publisher: PublicKey,
        subject: PublicKey,
        current: Option<(&RevocationSet, u64)>,
    ) -> Result<Vec<ResourceGrant>, Error> {
        let leaf = self.links(publisher, current)?;
        if leaf.extended()?.node != Some(crate::PeerId::of(subject))
            || leaf.kind != PrincipalKind::Node
            || leaf.delegations != 0
        {
            return Err(denied("operation/session requires a node credential"));
        }
        if current.is_some() && !leaf.extended()?.partitions.is_empty() {
            return Err(denied(
                "partition credentials require a concrete partition tuple",
            ));
        }
        Ok(leaf.grants.clone())
    }
    /// Checks a node's alias-bound grants without requiring its public node key.
    pub fn verify_reference_selectors(
        &self,
        publisher: PublicKey,
        node: crate::PeerId,
        revocations: &RevocationSet,
        time: u64,
    ) -> Result<(Vec<ResourceGrant>, Vec<crate::PartitionGrant>), Error> {
        let leaf = self.links(publisher, Some((revocations, time)))?;
        let extension = leaf.extended()?;
        if leaf.kind != PrincipalKind::Node || leaf.delegations != 0 || extension.node != Some(node)
        {
            return Err(denied("credential does not bind the node reference"));
        }
        Ok((leaf.grants.clone(), extension.partitions))
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
    /// parent's subject, narrows it in grants, validity, depth and author
    /// range, lives at most `MAX_CHAIN_LIFETIME` and, with `current`, is
    /// valid at its time and ended by no statement of its set. A link with
    /// issuer authority may delegate any grant and author range.
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
        for signed in &self.0 {
            let capability = &signed.capability;
            let extended = capability.extended()?;
            let stale = match current {
                Some((revocations, time)) => {
                    capability.not_before > time
                        || capability.expires <= time
                        || revocations.ends(signed)?
                }
                None => false,
            };
            let held = capability.grants.len() + extended.partitions.len();
            if signed.issuer != issuer
                || stale
                || capability.not_before >= capability.expires
                || capability.expires - capability.not_before > MAX_CHAIN_LIFETIME + LIFETIME_SKEW
                || capability.delegations as usize >= MAX_CREDENTIAL_DEPTH
                || (held == 0 && !extended.issues && extended.trusted.is_none())
                || held > 64
                || !signed.issuer.verify(
                    &signing_bytes(CAPABILITY_LABEL, capability)?,
                    &signed.signature,
                )
            {
                return Err(denied("invalid or revoked credential"));
            }
            if capability.grants.iter().any(|grant| grant.fields.named() > 256 || matches!(&grant.rows, rocky_core::RowSelection::Only(rows) if rows.is_empty() || rows.len() > 256)) {
                return Err(denied("credential resource quota"));
            }
            Self::shape(capability, &extended, parent.is_none())?;
            if let Some((parent, parent_extended)) = &parent {
                // Issuer authority grants anything; other links narrow.
                let authority = parent_extended.issues;
                if parent.kind == PrincipalKind::Node
                    || parent.delegations == 0
                    || capability.delegations >= parent.delegations
                    || capability.not_before < parent.not_before
                    || capability.expires > parent.expires
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
                        && extended.authors.is_some_and(|range| {
                            !parent_extended
                                .authors
                                .is_some_and(|parent| parent.includes(&range))
                        }))
                {
                    return Err(denied("delegation expands authority"));
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
    /// Node certificates carry exactly one number, attenuated from their
    /// identity's range, and name their subject. Only identities hold issuer
    /// authority.
    fn shape(capability: &Capability, extended: &Extended, _root: bool) -> Result<(), Error> {
        let valid = match capability.kind {
            PrincipalKind::Node => {
                extended
                    .authors
                    .is_some_and(|range| range.single().is_some())
                    && extended.node == Some(crate::PeerId::of(capability.subject))
                    && !extended.issues
                    && extended.trusted.is_none()
            }
            PrincipalKind::User => extended.node.is_none(),
            PrincipalKind::Deployment => {
                extended.node.is_none() && !extended.issues && extended.trusted.is_none()
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
        let leaf = self.links(publisher, None)?;
        if leaf.kind != PrincipalKind::Node
            || leaf.delegations != 0
            || leaf.subject != alias
            || leaf.extended()?.node != Some(node)
        {
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
}

/// Everything a device presents to use a domain, from any issuer: the
/// pinned publisher, the revocations it knows and the device's chain. It
/// names no generation; any generation of the domain admits it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Credentials {
    pub publisher: PublicKey,
    pub revocations: RevocationSet,
    pub chain: CredentialChain,
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
    /// Verifies the whole set for `device` of application `app` at `time`.
    /// Needs no database, endpoint or issuer.
    pub fn verify(&self, device: PublicKey, app: AppId, time: u64) -> Result<Verified, Error> {
        self.revocations.verify(self.publisher, app)?;
        let grants = self
            .chain
            .verify(self.publisher, device, &self.revocations, time)?;
        if grants.iter().any(|grant| grant.scope.domain.app != app) {
            return Err(denied("credentials for another application"));
        }
        Ok(Verified {
            principal: self
                .chain
                .principal()
                .ok_or_else(|| denied("credential depth"))?,
            device,
            revocations: self.revocations.digest(time)?,
            expires: self.chain.expires(),
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

    #[test]
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
        earlier.signature =
            publisher.sign(&signing_bytes("rocky.capability.v1", &earlier.capability).unwrap());
        let earlier = crate::device_chain(earlier, &user, node.public_key(), 1).unwrap();
        assert!(
            earlier
                .verify(publisher.public_key(), node.public_key(), &revocations, 200)
                .is_err()
        );
    }
}
