//! Signed manifests, capabilities and credential chains.
use crate::{
    AuthorRange, Error, Extended, Extension, LIFETIME_SKEW, MAX_CHAIN_LIFETIME,
    MAX_CREDENTIAL_BYTES, MAX_CREDENTIAL_DEPTH, MAX_REVOCATION_SET_BYTES, PrivateKey, Pseudonym,
    PublicKey, RevocationSet, decode_bounded, denied, encode, signing_bytes,
};
use p2panda_core::Signature;
use rocky_core::{ApplicationManifest, Commitment, PrincipalKind, ResourceGrant};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignedManifest {
    body: Vec<u8>,
    signature: Signature,
}
impl SignedManifest {
    pub fn sign(manifest: &ApplicationManifest, publisher: &PrivateKey) -> Result<Self, Error> {
        manifest.validate()?;
        if manifest.publisher != *publisher.public_key().as_bytes() {
            return Err(denied("manifest publisher mismatch"));
        }
        let body = manifest.canonical_body();
        let signature = publisher.sign(&signing_bytes("rocky.signed-manifest.v1", &body)?);
        Ok(Self { body, signature })
    }
    pub fn verify(&self, trusted_publisher: PublicKey) -> Result<ApplicationManifest, Error> {
        let manifest = ApplicationManifest::from_body(&self.body)?;
        if manifest.publisher != *trusted_publisher.as_bytes()
            || !trusted_publisher.verify(
                &signing_bytes("rocky.signed-manifest.v1", &self.body)?,
                &self.signature,
            )
        {
            return Err(denied("invalid manifest signature"));
        }
        Ok(manifest)
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        encode(self)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        decode_bounded(bytes, rocky_core::MAX_MANIFEST_BYTES * 2 + 1024)
    }
}

/// A publisher's transition from one signed manifest to the next. See
/// `docs/runtime/generations.md`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedTransition {
    body: Vec<u8>,
    signature: Signature,
}
impl SignedTransition {
    pub fn sign(
        transition: &rocky_core::GenerationTransition,
        publisher: &PrivateKey,
    ) -> Result<Self, Error> {
        transition.validate()?;
        let body = transition.canonical();
        let signature = publisher.sign(&signing_bytes("rocky.signed-transition.v1", &body)?);
        Ok(Self { body, signature })
    }
    pub fn verify(&self, publisher: PublicKey) -> Result<rocky_core::GenerationTransition, Error> {
        let transition = rocky_core::GenerationTransition::from_canonical(&self.body)?;
        if !publisher.verify(
            &signing_bytes("rocky.signed-transition.v1", &self.body)?,
            &self.signature,
        ) {
            return Err(denied("invalid transition signature"));
        }
        Ok(transition)
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        encode(self)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        decode_bounded(bytes, 1024)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    pub manifest: Commitment,
    pub subject: PublicKey,
    pub kind: PrincipalKind,
    pub grants: Vec<ResourceGrant>,
    pub not_before: u64,
    pub expires: u64,
    pub delegations: u8,
    /// Author ranges, pseudonym binding and partition selectors.
    pub extension: Extension,
}
impl Capability {
    /// The versioned authority fields of this capability.
    pub fn extended(&self) -> Result<Extended, Error> {
        self.extension.read()
    }
    /// The author numbers this link assigns, or a pseudonym holds.
    pub fn authors(&self) -> Result<Option<AuthorRange>, Error> {
        Ok(self.extended()?.authors)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignedCapability {
    /// Portable assignment provenance; carries no partition tuples.
    pub author_binding: crate::AuthorBinding,
    pub issuer: PublicKey,
    pub capability: Capability,
    signature: Signature,
}
impl SignedCapability {
    pub fn sign(capability: Capability, issuer: &PrivateKey) -> Result<Self, Error> {
        let signature = issuer.sign(&signing_bytes("rocky.capability.v1", &capability)?);
        Ok(Self {
            author_binding: crate::AuthorBinding::sign(&capability, issuer)?,
            issuer: issuer.public_key(),
            capability,
            signature,
        })
    }
    pub fn id(&self) -> Result<Commitment, Error> {
        Ok(rocky_core::commitment(
            "rocky.capability.id.v1",
            &encode(self)?,
        ))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CredentialChain(pub Vec<SignedCapability>);
impl CredentialChain {
    /// Concrete grants of a node under `revocations` at `time`.
    /// Partition leaves require `verify_partition` with a concrete tuple.
    pub fn verify(
        &self,
        publisher: PublicKey,
        manifest: Commitment,
        subject: PublicKey,
        revocations: &RevocationSet,
        time: u64,
    ) -> Result<Vec<ResourceGrant>, Error> {
        self.check(publisher, manifest, subject, Some((revocations, time)))
    }
    /// Whether the publisher once delegated to the node `subject` through
    /// this chain, whatever the time and later revocations. It
    /// proves who a renewal is for; it authorizes nothing. Returns the
    /// root capability.
    pub fn provenance(
        &self,
        publisher: PublicKey,
        manifest: Commitment,
        subject: PublicKey,
    ) -> Result<&Capability, Error> {
        self.check(publisher, manifest, subject, None)?;
        self.0
            .first()
            .map(|root| &root.capability)
            .ok_or_else(|| denied("credential depth"))
    }
    fn check(
        &self,
        publisher: PublicKey,
        manifest: Commitment,
        subject: PublicKey,
        current: Option<(&RevocationSet, u64)>,
    ) -> Result<Vec<ResourceGrant>, Error> {
        let leaf = self.links(publisher, manifest, current)?;
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
        manifest: Commitment,
        node: crate::PeerId,
        revocations: &RevocationSet,
        time: u64,
    ) -> Result<(Vec<ResourceGrant>, Vec<crate::PartitionGrant>), Error> {
        let leaf = self.links(publisher, manifest, Some((revocations, time)))?;
        let extension = leaf.extended()?;
        if leaf.kind != PrincipalKind::Node || leaf.delegations != 0 || extension.node != Some(node)
        {
            return Err(denied("credential does not bind the node reference"));
        }
        Ok((leaf.grants.clone(), extension.partitions))
    }
    /// Checks every link and returns the leaf: each is signed by its
    /// parent's subject, narrows it in grants, validity, depth and author
    /// range, lives at most `MAX_CHAIN_LIFETIME` and, with `current`, is
    /// valid at its time and ended by no statement of its set.
    pub(crate) fn links(
        &self,
        publisher: PublicKey,
        manifest: Commitment,
        current: Option<(&RevocationSet, u64)>,
    ) -> Result<&Capability, Error> {
        if self.0.is_empty() || self.0.len() > MAX_CREDENTIAL_DEPTH {
            return Err(denied("credential depth"));
        }
        let mut issuer = publisher;
        let mut parent: Option<(&Capability, Option<AuthorRange>)> = None;
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
            if signed.issuer != issuer
                || capability.manifest != manifest
                || stale
                || capability.not_before >= capability.expires
                || capability.expires - capability.not_before > MAX_CHAIN_LIFETIME + LIFETIME_SKEW
                || capability.delegations as usize >= MAX_CREDENTIAL_DEPTH
                || capability.grants.len() + extended.partitions.len() == 0
                || capability.grants.len() + extended.partitions.len() > 64
                || !signed.issuer.verify(
                    &signing_bytes("rocky.capability.v1", capability)?,
                    &signed.signature,
                )
            {
                return Err(denied("invalid or revoked credential"));
            }
            if capability.grants.iter().any(|grant| grant.fields.len() > 256 || matches!(&grant.rows, rocky_core::RowSelection::Only(rows) if rows.is_empty() || rows.len() > 256)) {
                return Err(denied("credential resource quota"));
            }
            Self::shape(capability, &extended, parent.is_none())?;
            if let Some((parent, authors)) = parent {
                let parent_partitions = parent.extended()?.partitions;
                if parent.kind == PrincipalKind::Node
                    || parent.delegations == 0
                    || capability.delegations >= parent.delegations
                    || capability.not_before < parent.not_before
                    || capability.expires > parent.expires
                    || (parent.kind == PrincipalKind::Deployment
                        && capability.kind == PrincipalKind::User)
                    || capability
                        .grants
                        .iter()
                        .any(|grant| !parent.grants.iter().any(|allowed| allowed.includes(grant)))
                    || extended.partitions.iter().any(|grant| {
                        !parent_partitions
                            .iter()
                            .any(|allowed| allowed.includes(grant))
                    })
                    || extended
                        .authors
                        .is_some_and(|range| !authors.is_some_and(|parent| parent.includes(&range)))
                {
                    return Err(denied("delegation expands authority"));
                }
            } else if capability.kind != PrincipalKind::User {
                return Err(denied("publisher must delegate to user"));
            }
            issuer = capability.subject;
            parent = Some((capability, extended.authors));
        }
        parent
            .map(|(leaf, _)| leaf)
            .ok_or_else(|| denied("missing leaf credential"))
    }
    /// Nodes carry selectors but no author/device binding; pseudonyms
    /// carry one number, a sealed device and grants on one table.
    fn shape(capability: &Capability, extended: &Extended, root: bool) -> Result<(), Error> {
        let valid = match capability.kind {
            PrincipalKind::Node => {
                extended.authors.is_none() && extended.device.is_none() && extended.node.is_some()
            }
            PrincipalKind::User | PrincipalKind::Deployment => {
                extended.device.is_none() && extended.node.is_none()
            }
            PrincipalKind::Pseudonym => {
                let first = capability
                    .grants
                    .first()
                    .map(|grant| (grant.scope.domain, grant.scope.table))
                    .or_else(|| {
                        extended
                            .partitions
                            .first()
                            .map(|grant| (grant.domain, grant.table))
                    });
                let Some((domain, table)) = first else {
                    return Err(denied("missing pseudonym grants"));
                };
                !root
                    && capability.delegations == 0
                    && extended.device.is_some()
                    && extended.node.is_none()
                    && extended
                        .authors
                        .is_some_and(|range| range.single().is_some())
                    && capability
                        .grants
                        .iter()
                        .all(|grant| grant.scope.domain == domain && grant.scope.table == table)
                    && extended
                        .partitions
                        .iter()
                        .all(|grant| grant.domain == domain && grant.table == table)
            }
        };
        if valid {
            Ok(())
        } else {
            Err(denied("invalid credential extension for its principal"))
        }
    }
    /// The pseudonym `key` under `revocations` at `time`: the chain ends
    /// at it as a pseudonym leaf. A node chain is refused.
    pub fn verify_pseudonym(
        &self,
        publisher: PublicKey,
        manifest: Commitment,
        key: PublicKey,
        revocations: &RevocationSet,
        time: u64,
    ) -> Result<Pseudonym, Error> {
        let leaf = self.links(publisher, manifest, Some((revocations, time)))?;
        if !leaf.extended()?.partitions.is_empty() {
            return Err(denied(
                "partition pseudonyms require a concrete partition tuple",
            ));
        }
        let pseudonym = self.pseudonym_leaf(publisher, manifest)?;
        if pseudonym.key != key {
            return Err(denied("the pseudonym credential names another key"));
        }
        Ok(pseudonym)
    }
    pub fn verify_pseudonym_selectors(
        &self,
        publisher: PublicKey,
        manifest: Commitment,
        key: PublicKey,
        revocations: &RevocationSet,
        time: u64,
    ) -> Result<(Pseudonym, Vec<crate::PartitionGrant>), Error> {
        let (_, partitions) = self.verify_selectors(publisher, manifest, key, revocations, time)?;
        Ok((self.pseudonym_leaf(publisher, manifest)?, partitions))
    }
    /// The receiver's check of an authored frame: `signer` holds exactly
    /// `author` in `table` under `revocations` at `time`. Returns its grants.
    #[allow(clippy::too_many_arguments)]
    pub fn verify_author(
        &self,
        publisher: PublicKey,
        manifest: Commitment,
        signer: PublicKey,
        author: u32,
        table: Commitment,
        revocations: &RevocationSet,
        time: u64,
    ) -> Result<Vec<ResourceGrant>, Error> {
        let pseudonym = self.verify_pseudonym(publisher, manifest, signer, revocations, time)?;
        if pseudonym.author != author || pseudonym.table != table {
            return Err(denied(
                "the author number does not belong to the signing key",
            ));
        }
        Ok(pseudonym.grants)
    }
    /// The pseudonym leaf of an authentic chain, whatever its time and
    /// revocations: what the publisher once let it hold.
    pub(crate) fn pseudonym_leaf(
        &self,
        publisher: PublicKey,
        manifest: Commitment,
    ) -> Result<Pseudonym, Error> {
        let leaf = self.links(publisher, manifest, None)?;
        if leaf.kind != PrincipalKind::Pseudonym {
            return Err(denied("the chain ends at no pseudonym"));
        }
        let extended = leaf.extended()?;
        let table = leaf
            .grants
            .first()
            .map(|grant| grant.scope.table)
            .or_else(|| extended.partitions.first().map(|grant| grant.table))
            .ok_or_else(|| denied("missing pseudonym grants"))?;
        let author = leaf
            .authors()?
            .and_then(|range| range.single())
            .ok_or_else(|| denied("invalid pseudonym"))?;
        let assigner = self
            .0
            .iter()
            .rev()
            .nth(1)
            .map(|parent| parent.capability.subject)
            .ok_or_else(|| denied("invalid pseudonym"))?;
        Ok(Pseudonym {
            key: leaf.subject,
            author,
            table,
            grants: leaf.grants.clone(),
            assigner,
            expires: self.expires(),
        })
    }
    /// Verifies a node's credential signing alias without revealing the node key.
    pub fn verify_node_alias(
        &self,
        publisher: PublicKey,
        manifest: Commitment,
        node: crate::PeerId,
        alias: PublicKey,
    ) -> Result<(), Error> {
        let leaf = self.links(publisher, manifest, None)?;
        if leaf.kind != PrincipalKind::Node
            || leaf.delegations != 0
            || leaf.subject != alias
            || leaf.extended()?.node != Some(node)
        {
            return Err(denied("credential alias does not bind the node reference"));
        }
        Ok(())
    }
    /// The node key of the device a pseudonym chain names, for a holder of
    /// the publisher key. Nobody else can open it.
    pub fn pseudonym_device(
        &self,
        publisher: &PrivateKey,
        manifest: Commitment,
    ) -> Result<PublicKey, Error> {
        let pseudonym = self.pseudonym_leaf(publisher.public_key(), manifest)?;
        let device = self
            .0
            .last()
            .map(|leaf| leaf.capability.extended())
            .transpose()?
            .and_then(|extended| extended.device)
            .ok_or_else(|| denied("invalid pseudonym"))?;
        device.open(publisher, pseudonym.key, pseudonym.author)
    }
    /// The user the chain delegates from: the login principal.
    pub fn principal(&self) -> Option<PublicKey> {
        self.0.first().map(|root| root.capability.subject)
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
/// pinned publisher, its signed manifest, the revocations it knows and the
/// device's chain.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Credentials {
    pub publisher: PublicKey,
    pub manifest: SignedManifest,
    pub revocations: RevocationSet,
    pub chain: CredentialChain,
}
/// What verified credentials establish.
#[derive(Clone, Debug)]
pub struct Verified {
    pub manifest: ApplicationManifest,
    pub principal: PublicKey,
    pub device: PublicKey,
    /// The digest of the live revocations verified against.
    pub revocations: Commitment,
    pub expires: u64,
    pub grants: Vec<ResourceGrant>,
}
impl Credentials {
    /// Verifies the whole set for `device` at `time`. Needs no database,
    /// endpoint or issuer.
    pub fn verify(&self, device: PublicKey, time: u64) -> Result<Verified, Error> {
        let manifest = self.manifest.verify(self.publisher)?;
        self.revocations.verify(self.publisher, manifest.app)?;
        let grants = self.chain.verify(
            self.publisher,
            manifest.hash(),
            device,
            &self.revocations,
            time,
        )?;
        Ok(Verified {
            manifest,
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
