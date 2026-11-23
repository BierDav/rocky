use crate::{
    Error, Extended, Extension, IdentityProvider, LIFETIME_SKEW, MAX_CHAIN_LIFETIME,
    MAX_CREDENTIAL_BYTES, MAX_CREDENTIAL_DEPTH, MAX_REVOCATION_SET_BYTES, PrivateKey, PublicKey,
    Purpose, RevocationSet, decode_bounded, denied, encode, signing_bytes,
};
use p2panda_core::Signature;
use rocky_core::{AppId, ApplicationManifest, Commitment, PrincipalKind, ResourceGrant};
use serde::{Deserialize, Serialize};

pub const MAX_ENDORSEMENT_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Certificate {
    pub capability: Capability,
    signature: Signature,
}
impl Certificate {
    pub fn of(link: &SignedCapability) -> Result<Self, Error> {
        if link.attestation.certificate.is_some() || link.capability.kind != PrincipalKind::Node {
            return Err(denied("not a node certificate"));
        }
        Ok(Self {
            capability: link.capability.clone(),
            signature: link.attestation.signature,
        })
    }
    pub fn node_of(&self, identity: PublicKey) -> Result<PublicKey, Error> {
        check_node(&self.capability, identity)?;
        if !identity.verify(
            &signing_bytes(CAPABILITY_LABEL, &self.capability)?,
            &self.signature,
        ) {
            return Err(denied("invalid node certificate"));
        }
        Ok(self.capability.subject)
    }
    pub fn claimed_node(&self) -> PublicKey {
        self.capability.subject
    }
}
fn check_node(capability: &Capability, identity: PublicKey) -> Result<(), Error> {
    let extended = capability.extended()?;
    if capability.kind != PrincipalKind::Node
        || capability.delegations != 0
        || !capability.grants.is_empty()
        || capability.expires != crate::NEVER
        || !extended.partitions.is_empty()
        || extended.node != Some(crate::PeerId::of(capability.subject))
        || extended.issues
        || extended.families
        || extended.trusted.is_some()
        || extended.claims.is_some()
        || extended.origin.is_some()
        || extended.database.is_none()
        || extended
            .sequence
            .is_none_or(|sequence| (sequence == 0) != extended.previous.is_none())
    {
        return Err(denied("invalid node certificate"));
    }
    crate::possession::check(capability, identity)
}

#[derive(Clone)]
pub enum Signer<'a> {
    Key(&'a dyn IdentityProvider),
    Node {
        key: &'a PrivateKey,
        identity: PublicKey,
        certificate: Box<Certificate>,
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
            certificate: Box::new(certificate),
        })
    }
    pub fn principal(&self) -> Result<PublicKey, Error> {
        match self {
            Self::Key(key) => key.public_key(),
            Self::Node { identity, .. } => Ok(*identity),
        }
    }
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
                certificate: Some(certificate.as_ref().clone()),
                signature: key.sign(message),
            },
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attestation {
    certificate: Option<Certificate>,
    signature: Signature,
}
impl Attestation {
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
    pub(crate) fn node_ended(
        &self,
        (revocations, time): (&RevocationSet, u64),
        protected: bool,
        identity: PublicKey,
    ) -> bool {
        self.certificate.as_ref().is_some_and(|certificate| {
            certificate.capability.not_before > time
                || revocations.ends_node_of(identity, certificate.claimed_node(), protected)
        })
    }
    pub(crate) fn link_signer_ended(
        &self,
        (revocations, time): (&RevocationSet, u64),
        protected: bool,
        identity: PublicKey,
    ) -> bool {
        self.node_ended((revocations, time), protected, identity)
    }
    pub fn certificate(&self) -> Option<&Certificate> {
        self.certificate.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endorsement {
    pub chain: CredentialChain,
    attestation: Attestation,
}
impl Endorsement {
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
    pub fn verify_historic<T: Serialize>(
        &self,
        publisher: PublicKey,
        label: &str,
        value: &T,
    ) -> Result<PublicKey, Error> {
        self.check(publisher, label, value, None)
    }
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
    pub fn verify_unrevoked<T: Serialize>(
        &self,
        publisher: PublicKey,
        label: &str,
        value: &T,
        revocations: &RevocationSet,
    ) -> Result<PublicKey, Error> {
        self.check(
            publisher,
            label,
            value,
            Some((revocations, self.whole_chain_valid_from())),
        )
    }
    fn whole_chain_valid_from(&self) -> u64 {
        self.chain
            .0
            .iter()
            .flat_map(|link| {
                [
                    Some(link.capability.not_before),
                    link.attestation
                        .certificate()
                        .map(|certificate| certificate.capability.not_before),
                ]
            })
            .flatten()
            .chain(
                self.attestation
                    .certificate()
                    .map(|certificate| certificate.capability.not_before),
            )
            .max()
            .unwrap_or(0)
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
    fn attested(
        &self,
        signer: PublicKey,
        message: &[u8],
        current: Option<(&RevocationSet, u64)>,
    ) -> Result<Option<PublicKey>, Error> {
        if self.chain.0.is_empty() != self.attestation.certificate.is_none() {
            return Err(denied("only the owner signs with its own key"));
        }
        if !self.chain.0.is_empty() {
            let database = self.chain.database_context(self.chain.0[0].issuer)?;
            if self
                .attestation
                .certificate()
                .and_then(|certificate| certificate.capability.extended().ok())
                .and_then(|extended| extended.database)
                != Some(database)
            {
                return Err(denied("endorsement belongs to another database"));
            }
        }
        let node = self.attestation.verify(signer, message)?;
        let only_root_revokes = self
            .chain
            .trusted_subject()
            .is_ok_and(|trusted| trusted == Some(signer));
        if current.is_some_and(|current| {
            self.attestation
                .node_ended(current, only_root_revokes, signer)
        }) {
            return Err(denied("the signing node is revoked"));
        }
        Ok(node)
    }
    pub(crate) fn verify_tip_statement<T: Serialize>(
        &self,
        publisher: PublicKey,
        scope: rocky_core::Scope,
        label: &str,
        value: &T,
        current: Option<(&RevocationSet, u64)>,
    ) -> Result<(bool, Option<PublicKey>), Error> {
        if !self.chain.0.is_empty()
            && self.chain.database_context(publisher)?
                != (crate::DatabaseContext { publisher, scope })
        {
            return Err(denied("tip endorsement belongs to another database"));
        }
        let (signer, trusted) = if self.chain.0.is_empty() {
            (publisher, true)
        } else {
            let leaf = self.chain.links(publisher, current)?;
            if leaf.kind != PrincipalKind::User {
                return Err(denied("tip statements require an identity"));
            }
            let trusted = self.chain.0.len() == 1 && leaf.extended()?.trusted == Some(scope);
            let issues_in_this_domain = |link: &SignedCapability| {
                link.capability
                    .extended()
                    .is_ok_and(|e| e.issues && e.trusted.is_none_or(|held| held == scope))
            };
            if !trusted && !self.chain.0.iter().all(issues_in_this_domain) {
                return Err(denied("the chain ends at no issuer"));
            }
            (leaf.subject, trusted)
        };
        let node = self
            .attested(signer, &signing_bytes(label, value)?, current)
            .map_err(|_| denied("invalid tip statement signature"))?;
        Ok((trusted, node))
    }
    pub fn claimed_signer_of(chain: &CredentialChain) -> Option<PublicKey> {
        chain.0.last().map(|last| last.capability.subject)
    }
    pub fn attestation(&self) -> &Attestation {
        &self.attestation
    }
    pub fn claimed_signer(&self, publisher: PublicKey) -> PublicKey {
        Self::claimed_signer_of(&self.chain).unwrap_or(publisher)
    }
    pub fn claimed_node(&self) -> Option<PublicKey> {
        self.attestation
            .certificate
            .as_ref()
            .map(Certificate::claimed_node)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedManifest {
    body: Vec<u8>,
    endorsement: Endorsement,
}
const MANIFEST: &str = "rocky.signed-manifest.v5";
impl SignedManifest {
    pub fn sign(
        manifest: &ApplicationManifest,
        owner: &dyn IdentityProvider,
    ) -> Result<Self, Error> {
        Self::endorse(manifest, owner, &CredentialChain(Vec::new()))
    }
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
        signed.verify_historic(publisher)?;
        Ok(signed)
    }
    pub fn verify_historic(
        &self,
        trusted_publisher: PublicKey,
    ) -> Result<ApplicationManifest, Error> {
        let manifest = ApplicationManifest::from_body(&self.body)?;
        if manifest.publisher != *trusted_publisher.as_bytes() {
            return Err(denied("invalid manifest signature"));
        }
        self.endorsement
            .verify_historic(trusted_publisher, MANIFEST, &self.body)
            .map_err(|_| denied("invalid manifest signature"))?;
        Ok(manifest)
    }
    pub fn descriptor(&self) -> Commitment {
        rocky_core::commitment("rocky.database-descriptor.v2", &self.body)
    }
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
            rocky_core::MAX_MANIFEST_BYTES * 2 + 2048 + MAX_ENDORSEMENT_BYTES,
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    pub subject: PublicKey,
    pub kind: PrincipalKind,
    pub grants: Vec<ResourceGrant>,
    pub not_before: u64,
    pub expires: u64,
    pub delegations: u8,
    pub extension: Extension,
}
impl Capability {
    pub fn extended(&self) -> Result<Extended, Error> {
        self.extension.read()
    }
}
const CAPABILITY_LABEL: &str = "rocky.capability.v3";
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedCapability {
    pub author_binding: crate::AuthorBinding,
    pub issuer: PublicKey,
    pub capability: Capability,
    attestation: Attestation,
}
impl SignedCapability {
    pub fn sign<'s>(capability: Capability, signer: impl Into<Signer<'s>>) -> Result<Self, Error> {
        let signer = signer.into();
        if capability.grants.len() + capability.extended()?.partitions.len() > crate::MAX_GRANTS {
            return Err(denied("credential grant quota"));
        }
        let purpose = match capability.kind {
            PrincipalKind::Node if signer.signing_node().is_some() => {
                return Err(denied("only an identity's own key certifies its node"));
            }
            PrincipalKind::Node => Purpose::NodeCertificate,
            _ => Purpose::Capability,
        };
        if capability.kind == PrincipalKind::Node {
            check_node(&capability, signer.principal()?)?;
        }
        Self::attest(capability, &signer, purpose)
    }
    fn attest(
        capability: Capability,
        signer: &Signer<'_>,
        purpose: Purpose,
    ) -> Result<Self, Error> {
        Ok(Self {
            author_binding: crate::AuthorBinding::sign(&capability, signer)?,
            issuer: signer.principal()?,
            attestation: signer.attest(purpose, &signing_bytes(CAPABILITY_LABEL, &capability)?)?,
            capability,
        })
    }
    #[cfg(test)]
    pub(crate) fn sign_unchecked_node(
        capability: Capability,
        identity: &dyn IdentityProvider,
    ) -> Self {
        Self::attest(capability, &Signer::Key(identity), Purpose::NodeCertificate)
            .expect("an identity signs any capability")
    }
    pub fn id(&self) -> Result<Commitment, Error> {
        Ok(rocky_core::commitment(
            "rocky.capability.id.v3",
            &encode(self)?,
        ))
    }
    pub fn attestation(&self) -> &Attestation {
        &self.attestation
    }
    fn verify_signature(&self) -> Result<Option<PublicKey>, Error> {
        if self.capability.kind == PrincipalKind::Node && self.attestation.certificate.is_some() {
            return Err(denied("only an identity's own key certifies its node"));
        }
        self.attestation.verify(
            self.issuer,
            &signing_bytes(CAPABILITY_LABEL, &self.capability)?,
        )
    }
    pub fn certifies_in(
        &self,
        database: crate::DatabaseContext,
        node: PublicKey,
        time: u64,
    ) -> Result<PublicKey, Error> {
        if self.capability.extended()?.database != Some(database) {
            return Err(denied("certificate belongs to another database"));
        }
        self.certifies(node, time)
    }
    pub fn certifies(&self, node: PublicKey, time: u64) -> Result<PublicKey, Error> {
        if Certificate::of(self)
            .and_then(|certificate| certificate.node_of(self.issuer))
            .ok()
            != Some(node)
            || self.capability.not_before > time
        {
            return Err(denied("not a current certificate of this node"));
        }
        self.author_binding.verify_historic(self.issuer, node)?;
        Ok(self.issuer)
    }

    pub fn membership(&self) -> Result<Option<(u64, Option<Commitment>)>, Error> {
        let extended = self.capability.extended()?;
        Ok(extended
            .sequence
            .map(|sequence| (sequence, extended.previous)))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialChain(pub Vec<SignedCapability>);
#[derive(Clone, Debug)]
pub struct VerifiedChain {
    pub(crate) leaf: Capability,
    pub(crate) held: Capability,
    families: crate::GrantFamilies,
    revocations: Commitment,
    not_before: u64,
    expires: u64,
}
impl VerifiedChain {
    pub fn partition(
        &self,
        subject: PublicKey,
        scope: rocky_core::ResourceScope,
        tuple: &rocky_core::PartitionTuple,
        revocations: &RevocationSet,
        time: u64,
    ) -> Result<Vec<ResourceGrant>, Error> {
        self.valid_node(subject, revocations, time)?;
        let (mut grants, partitions) = self.grants_for(scope.table)?;
        grants.retain(|grant| grant.scope == scope);
        for selector in partitions {
            if let Some(grant) = selector.resolve(scope, tuple)? {
                grants.push(grant);
            }
        }
        if grants.is_empty() {
            return Err(denied("no grant for this partition"));
        }
        Ok(grants)
    }
    pub fn selectors(
        &self,
        subject: PublicKey,
        revocations: &RevocationSet,
        time: u64,
    ) -> Result<(Vec<ResourceGrant>, Vec<crate::PartitionGrant>), Error> {
        self.valid_node(subject, revocations, time)?;
        Ok((
            self.families.resources(&self.held.grants),
            self.families.partitions(&self.held.extended()?.partitions),
        ))
    }
    pub(crate) fn valid_node(
        &self,
        subject: PublicKey,
        revocations: &RevocationSet,
        time: u64,
    ) -> Result<(), Error> {
        if revocations.fingerprint()? != self.revocations {
            return Err(denied(
                "the chain was verified under another revocation set",
            ));
        }
        if time < self.not_before
            || time >= self.expires
            || self.leaf.kind != PrincipalKind::Node
            || self.leaf.subject != subject
        {
            return Err(denied("invalid or expired node credential"));
        }
        Ok(())
    }
    pub(crate) fn grants_for(
        &self,
        table: Commitment,
    ) -> Result<(Vec<ResourceGrant>, Vec<crate::PartitionGrant>), Error> {
        let database = self
            .leaf
            .extended()?
            .database
            .ok_or_else(|| denied("node requires a database context"))?;
        Ok((
            self.families.resources_for_table(&self.held.grants, table),
            self.families
                .partitions_for(&self.held.extended()?.partitions, database.scope, table),
        ))
    }
    pub fn author(
        &self,
        subject: PublicKey,
        table: Commitment,
        revocations: &RevocationSet,
        time: u64,
    ) -> Result<Vec<ResourceGrant>, Error> {
        self.valid_node(subject, revocations, time)?;
        let (grants, partitions) = self.grants_for(table)?;
        if partitions.iter().any(|grant| grant.table == table) {
            return Err(denied("partition tuple required"));
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
    pub fn verified(
        &self,
        publisher: PublicKey,
        revocations: &RevocationSet,
        time: u64,
    ) -> Result<VerifiedChain, Error> {
        self.verified_with_families(
            publisher,
            revocations,
            time,
            &crate::GrantFamilies::default(),
        )
    }
    pub fn verified_with_families(
        &self,
        publisher: PublicKey,
        revocations: &RevocationSet,
        time: u64,
        families: &crate::GrantFamilies,
    ) -> Result<VerifiedChain, Error> {
        let (leaf, held) =
            self.node_with_families(publisher, Some((revocations, time)), families)?;
        Ok(VerifiedChain {
            leaf: leaf.clone(),
            held: held.clone(),
            families: families.for_capability(held)?,
            revocations: revocations.fingerprint()?,
            not_before: self
                .0
                .iter()
                .map(|link| link.capability.not_before)
                .max()
                .unwrap_or(u64::MAX),
            expires: self.expires(),
        })
    }

    pub fn verify(
        &self,
        publisher: PublicKey,
        subject: PublicKey,
        revocations: &RevocationSet,
        time: u64,
    ) -> Result<Vec<ResourceGrant>, Error> {
        self.check(publisher, subject, Some((revocations, time)))
    }
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
    pub fn historic_provenance(
        &self,
        publisher: PublicKey,
        subject: PublicKey,
    ) -> Result<&Capability, Error> {
        self.check(publisher, subject, None)?;
        self.identity_index()
            .map(|index| &self.0[index].capability)
            .ok_or_else(|| denied("credential depth"))
    }
    pub fn historic_provenance_with_families(
        &self,
        publisher: PublicKey,
        subject: PublicKey,
        families: &crate::GrantFamilies,
    ) -> Result<&Capability, Error> {
        let (leaf, _) = self.node_with_families(publisher, None, families)?;
        if leaf.subject != subject {
            return Err(denied("operation/session requires a node credential"));
        }
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
    pub fn database_context(&self, publisher: PublicKey) -> Result<crate::DatabaseContext, Error> {
        let first = self
            .0
            .first()
            .ok_or_else(|| denied("missing database authority"))?;
        if first.issuer != publisher {
            return Err(denied("another database publisher"));
        }
        for signed in &self.0 {
            if signed.capability.kind == PrincipalKind::Node {
                continue;
            }
            let extended = signed.capability.extended()?;
            if let Some(database) = extended.database {
                if database.publisher != publisher {
                    return Err(denied("database publisher mismatch"));
                }
                return Ok(database);
            }
            let scope = extended
                .trusted
                .or_else(|| {
                    signed
                        .capability
                        .grants
                        .first()
                        .map(|grant| grant.scope.domain)
                })
                .or_else(|| extended.partitions.first().map(|grant| grant.domain))
                .or_else(|| extended.claims.map(|claims| claims.scope));
            if let Some(scope) = scope {
                return Ok(crate::DatabaseContext { publisher, scope });
            }
        }
        Err(denied("missing database namespace"))
    }
    pub fn verify_database(&self, expected: crate::DatabaseContext) -> Result<(), Error> {
        if self.database_context(expected.publisher)? != expected
            || self.certificate().is_none_or(|certificate| {
                certificate
                    .capability
                    .extended()
                    .ok()
                    .and_then(|extended| extended.database)
                    != Some(expected)
            })
        {
            return Err(denied("credential belongs to another database"));
        }
        Ok(())
    }
    pub(crate) fn node(
        &self,
        publisher: PublicKey,
        current: Option<(&RevocationSet, u64)>,
    ) -> Result<(&Capability, &Capability), Error> {
        self.node_with_families(publisher, current, &crate::GrantFamilies::default())
    }
    fn node_with_families(
        &self,
        publisher: PublicKey,
        current: Option<(&RevocationSet, u64)>,
        families: &crate::GrantFamilies,
    ) -> Result<(&Capability, &Capability), Error> {
        let leaf = self.links_with_families(publisher, current, families)?;
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
        self.verify_database(self.database_context(publisher)?)?;
        Ok((leaf, held))
    }
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
    pub(crate) fn links(
        &self,
        publisher: PublicKey,
        current: Option<(&RevocationSet, u64)>,
    ) -> Result<&Capability, Error> {
        self.links_with_families(publisher, current, &crate::GrantFamilies::default())
    }
    pub(crate) fn links_with_families(
        &self,
        publisher: PublicKey,
        current: Option<(&RevocationSet, u64)>,
        families: &crate::GrantFamilies,
    ) -> Result<&Capability, Error> {
        if self.0.is_empty() || self.0.len() > MAX_CREDENTIAL_DEPTH {
            return Err(denied("credential depth"));
        }
        let database = self.database_context(publisher)?;
        let mut issuer = publisher;
        let mut parent: Option<(&Capability, Extended)> = None;
        for (index, signed) in self.0.iter().enumerate() {
            let capability = &signed.capability;
            let extended = capability.extended()?;
            if extended.families && !families.authenticated() {
                return Err(denied("grant families require an authenticated schema"));
            }
            let node = capability.kind == PrincipalKind::Node;
            if capability
                .grants
                .iter()
                .any(|grant| grant.scope.domain != database.scope)
                || extended
                    .partitions
                    .iter()
                    .any(|grant| grant.domain != database.scope)
                || extended
                    .trusted
                    .is_some_and(|scope| scope != database.scope)
                || extended
                    .claims
                    .as_ref()
                    .is_some_and(|claims| claims.scope != database.scope)
                || extended.database.is_some_and(|context| context != database)
                || signed.attestation.certificate().is_some_and(|certificate| {
                    certificate
                        .capability
                        .extended()
                        .ok()
                        .and_then(|extended| extended.database)
                        != Some(database)
                })
            {
                return Err(denied("credential belongs to another database"));
            }
            let stale = match current {
                Some((revocations, time)) => {
                    capability.not_before > time
                        || capability.expires <= time
                        || revocations.ends_at(signed, self.protected(index)?)?
                        || signed.attestation.link_signer_ended(
                            (revocations, time),
                            self.protected_signer(index)?,
                            signed.issuer,
                        )
                }
                None => false,
            };
            let lives_until_root_revokes = match &parent {
                None => extended.trusted.is_some() && extended.issues,
                Some((parent, _)) => parent.expires == crate::NEVER && self.protected(index)?,
            };
            let lifetime =
                if node || (capability.expires == crate::NEVER && lives_until_root_revokes) {
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
                || held > crate::MAX_GRANTS
                || (extended.claims.is_some()
                    && (held != 0
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
            Self::shape(capability, &extended, signed.issuer)?;
            if let Some((parent, parent_extended)) = &parent {
                let authority = parent_extended.issues;
                let narrows_grants = !authority && !node;
                let parent_families = families.enabled(narrows_grants && parent_extended.families);
                let child_families = families.enabled(narrows_grants && extended.families);
                let (allowed, allowed_partitions, requested, requested_partitions) =
                    if narrows_grants {
                        (
                            parent_families.resources(&parent.grants),
                            parent_families.partitions(&parent_extended.partitions),
                            child_families.resources(&capability.grants),
                            child_families.partitions(&extended.partitions),
                        )
                    } else {
                        (Vec::new(), Vec::new(), Vec::new(), Vec::new())
                    };
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
                        && requested
                            .iter()
                            .any(|grant| !allowed.iter().any(|allowed| allowed.includes(grant))))
                    || (!authority
                        && requested_partitions.iter().any(|grant| {
                            !allowed_partitions
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
                {
                    return Err(denied("delegation expands authority"));
                }
                let signer_grant_time = if parent_extended.trusted.is_some() {
                    Some((parent.subject, parent.not_before))
                } else {
                    parent_extended
                        .origin
                        .map(|held| (held.identity, held.since))
                };
                if extended.origin.is_some_and(|origin| {
                    signer_grant_time.is_some_and(|(identity, since)| {
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
    fn protected_signer(&self, index: usize) -> Result<bool, Error> {
        Ok(self
            .trusted_subject()?
            .is_some_and(|trusted| self.0[index].issuer == trusted))
    }
    fn shape(capability: &Capability, extended: &Extended, issuer: PublicKey) -> Result<(), Error> {
        if extended.families
            && (extended.issues || extended.trusted.is_some() || extended.claims.is_some())
        {
            return Err(denied("grant families apply only to collection authority"));
        }
        let valid = match capability.kind {
            PrincipalKind::Node => check_node(capability, issuer).is_ok(),
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
    pub fn verify_author(
        &self,
        publisher: PublicKey,
        signer: PublicKey,
        table: Commitment,
        revocations: &RevocationSet,
        time: u64,
    ) -> Result<Vec<ResourceGrant>, Error> {
        let (grants, partitions) = self.verify_selectors(publisher, signer, revocations, time)?;
        if partitions.iter().any(|grant| grant.table == table) {
            return Err(denied("partition tuple required"));
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
    pub fn principal(&self) -> Option<PublicKey> {
        let leaf = self.0.last()?;
        match leaf.capability.kind {
            PrincipalKind::Node => Some(leaf.issuer),
            _ => Some(leaf.capability.subject),
        }
    }
    pub fn expires(&self) -> u64 {
        self.0
            .iter()
            .map(|signed| signed.capability.expires)
            .min()
            .unwrap_or(0)
    }
    pub fn certificate(&self) -> Option<&SignedCapability> {
        self.0
            .last()
            .filter(|leaf| leaf.capability.kind == PrincipalKind::Node)
    }
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
    pub fn capabilities(&self) -> Self {
        match self.identity_index() {
            Some(index) => Self(self.0[..=index].to_vec()),
            None => self.delegable(),
        }
    }
    pub fn with_certificate(&self, certificate: SignedCapability) -> Self {
        let mut chain = self.delegable();
        chain.0.push(certificate);
        chain
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Credentials {
    pub publisher: PublicKey,
    pub revocations: RevocationSet,
    pub capabilities: CredentialChain,
    pub certificate: Option<SignedCapability>,
}
#[derive(Clone, Debug)]
pub struct Verified {
    pub principal: PublicKey,
    pub device: PublicKey,
    pub revocations: Commitment,
    pub expires: u64,
    pub grants: Vec<ResourceGrant>,
}
impl Credentials {
    pub fn chain(&self) -> CredentialChain {
        let mut chain = self.capabilities.clone();
        chain.0.extend(self.certificate.clone());
        chain
    }
    pub fn of(publisher: PublicKey, revocations: RevocationSet, chain: &CredentialChain) -> Self {
        Self {
            publisher,
            revocations,
            capabilities: chain.delegable(),
            certificate: chain.certificate().cloned(),
        }
    }
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
    pub fn verify_in(
        &self,
        device: PublicKey,
        database: crate::DatabaseContext,
        time: u64,
    ) -> Result<Verified, Error> {
        if self.publisher != database.publisher {
            return Err(denied("another database publisher"));
        }
        self.revocations.verify_in(database)?;
        self.chain().verify_database(database)?;
        self.verify(device, database.scope.app, time)
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

    fn granted_device(
        publisher: &PrivateKey,
        user: &PrivateKey,
        node: &PrivateKey,
    ) -> (SignedCapability, CredentialChain) {
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
        let root = crate::Authority::owner(publisher)
            .issue(
                user.public_key(),
                vec![grant],
                crate::Validity {
                    issued_at: 100,
                    lifetime: std::time::Duration::from_secs(1000),
                    clock_skew: std::time::Duration::from_secs(0),
                },
            )
            .unwrap();
        let chain = crate::device_chain(root.clone(), user, node).unwrap();
        (root, chain)
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn a_capability_of_the_earlier_format_is_refused_and_names_no_generation() {
        let (publisher, user, node) = (PrivateKey::new(), PrivateKey::new(), PrivateKey::new());
        let (root, chain) = granted_device(&publisher, &user, &node);
        let revocations = RevocationSet::new();
        assert!(
            chain
                .verify(publisher.public_key(), node.public_key(), &revocations, 200)
                .is_ok()
        );
        let mut earlier = root;
        earlier.attestation.signature =
            publisher.sign(&signing_bytes("rocky.capability.v2", &earlier.capability).unwrap());
        let earlier = crate::device_chain(earlier, &user, &node).unwrap();
        assert!(
            earlier
                .verify(publisher.public_key(), node.public_key(), &revocations, 200)
                .is_err()
        );
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn a_node_certificate_naming_an_allocated_author_is_refused_by_verifiers() {
        let (publisher, user, node) = (PrivateKey::new(), PrivateKey::new(), PrivateKey::new());
        let (_, chain) = granted_device(&publisher, &user, &node);
        let revocations = RevocationSet::new();
        assert!(
            chain
                .verify(publisher.public_key(), node.public_key(), &revocations, 200)
                .is_ok()
        );
        let mut links = chain.0;
        let leaf = links.pop().unwrap();
        let mut extended = leaf.capability.extended().unwrap();
        extended.node = Some(crate::PeerId::from_bytes(rocky_core::commitment(
            "issuer-allocated author",
            &[1],
        )));
        let capability = Capability {
            extension: Extension::new(extended).unwrap(),
            ..leaf.capability
        };
        let signer = Signer::from(&user);
        links.push(SignedCapability {
            author_binding: crate::AuthorBinding::sign(&capability, &signer).unwrap(),
            issuer: leaf.issuer,
            attestation: signer
                .attest(
                    Purpose::NodeCertificate,
                    &signing_bytes(CAPABILITY_LABEL, &capability).unwrap(),
                )
                .unwrap(),
            capability,
        });
        assert!(
            CredentialChain(links)
                .verify(publisher.public_key(), node.public_key(), &revocations, 200)
                .is_err(),
            "a node certificate signed around the signing check still names its author by its own key"
        );
    }
}
