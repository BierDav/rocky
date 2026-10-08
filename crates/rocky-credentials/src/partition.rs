//! Partition grants and recipient-bound key envelopes; no runtime or storage.
use crate::{
    Error, PrivateKey, PublicKey, Signature, decode_bounded, denied, encode, signing_bytes,
};
use curve25519_dalek::{edwards::CompressedEdwardsY, montgomery::MontgomeryPoint};
use rand::RngCore;
use rocky_core::{
    Commitment, PartitionSelector, PartitionTuple, ResourceGrant, ResourceScope, Right,
    RowSelection, Scope, commitment,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha512};
use std::collections::BTreeSet;

/// A raw grant whose row/field rights remain paired across selected partitions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartitionGrant {
    pub domain: Scope,
    pub table: Commitment,
    pub selector: PartitionSelector,
    pub rows: RowSelection,
    pub rights: BTreeSet<Right>,
    pub fields: BTreeSet<Commitment>,
}
impl PartitionGrant {
    pub fn validate(&self) -> Result<(), Error> {
        self.selector
            .validate()
            .map_err(|error| Error::Denied(error.to_string()))?;
        if self.rights.is_empty()
            || self.fields.len() > 256
            || matches!(&self.rows, RowSelection::Only(rows) if rows.is_empty() || rows.len() > 256)
        {
            return Err(denied("partition grant quota or missing rights"));
        }
        Ok(())
    }
    pub fn includes(&self, other: &Self) -> bool {
        self.validate().is_ok()
            && other.validate().is_ok()
            && self.domain == other.domain
            && self.table == other.table
            && self.selector.includes(&other.selector)
            && self.rows.includes(&other.rows)
            && other.rights.is_subset(&self.rights)
            && other.fields.is_subset(&self.fields)
    }
    /// A concrete grant only when the selector matches the authenticated tuple.
    pub fn resolve(
        &self,
        domain: Scope,
        table: Commitment,
        tuple: &PartitionTuple,
    ) -> Result<Option<ResourceGrant>, Error> {
        self.validate()?;
        let scope = tuple
            .scope(domain, table)
            .map_err(|error| Error::Denied(error.to_string()))?;
        Ok(
            (self.domain == domain && self.table == table && self.selector.matches(tuple)).then(
                || ResourceGrant {
                    scope,
                    rows: self.rows.clone(),
                    rights: self.rights.clone(),
                    fields: self.fields.clone(),
                },
            ),
        )
    }
}

/// A partition key wrapped to one recipient, signed by the delivering issuer.
/// Possession establishes no credential right.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartitionKeyEnvelope {
    body: EnvelopeBody,
    signature: Signature,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvelopeBody {
    version: u32,
    manifest: Commitment,
    scope: ResourceScope,
    tuple: PartitionTuple,
    epoch: u64,
    issuer: PublicKey,
    recipient: PublicKey,
    ephemeral: [u8; 32],
    sealed: Vec<u8>,
}
#[derive(Serialize)]
struct Context<'a> {
    version: u32,
    manifest: &'a Commitment,
    scope: &'a ResourceScope,
    tuple: &'a PartitionTuple,
    epoch: u64,
    issuer: &'a PublicKey,
    recipient: &'a PublicKey,
    ephemeral: &'a [u8; 32],
}
impl EnvelopeBody {
    fn aad(&self) -> Result<Vec<u8>, Error> {
        signing_bytes(
            "rocky.partition-key-envelope.context.v1",
            &Context {
                version: self.version,
                manifest: &self.manifest,
                scope: &self.scope,
                tuple: &self.tuple,
                epoch: self.epoch,
                issuer: &self.issuer,
                recipient: &self.recipient,
                ephemeral: &self.ephemeral,
            },
        )
    }
    fn wrapping_key(&self, shared: [u8; 32]) -> Result<Commitment, Error> {
        if shared == [0; 32] {
            return Err(denied("invalid partition envelope DH key"));
        }
        Ok(commitment(
            "rocky.partition-key-envelope.key.v1",
            &[shared.as_slice(), &self.aad()?].concat(),
        ))
    }
    fn validate(&self) -> Result<(), Error> {
        if self.version != 1 {
            return Err(denied(
                "unsupported partition key envelope version; upgrade",
            ));
        }
        let scope = self
            .tuple
            .scope(self.scope.domain, self.scope.table)
            .map_err(|error| Error::Denied(error.to_string()))?;
        if self.epoch == 0 || scope != self.scope || self.sealed.len() != 60 {
            return Err(denied("invalid partition key envelope metadata"));
        }
        Ok(())
    }
}
impl PartitionKeyEnvelope {
    /// Wraps one partition epoch to each distinct recipient; refuses an empty set.
    pub fn seal_many(
        issuer: &PrivateKey,
        manifest: Commitment,
        scope: ResourceScope,
        tuple: PartitionTuple,
        epoch: u64,
        key: [u8; 32],
        recipients: &BTreeSet<PublicKey>,
    ) -> Result<Vec<Self>, Error> {
        if recipients.is_empty() || recipients.len() > 256 {
            return Err(denied("a partition envelope needs 1 to 256 recipients"));
        }
        recipients
            .iter()
            .map(|recipient| {
                Self::seal(
                    issuer,
                    manifest,
                    scope,
                    tuple.clone(),
                    epoch,
                    key,
                    *recipient,
                )
            })
            .collect()
    }
    pub fn seal(
        issuer: &PrivateKey,
        manifest: Commitment,
        scope: ResourceScope,
        tuple: PartitionTuple,
        epoch: u64,
        key: [u8; 32],
        recipient: PublicKey,
    ) -> Result<Self, Error> {
        let point = CompressedEdwardsY(*recipient.as_bytes())
            .decompress()
            .filter(|point| point.is_torsion_free() && !point.is_small_order())
            .ok_or_else(|| denied("invalid partition envelope recipient"))?
            .to_montgomery();
        let mut secret = [0; 32];
        rand::rngs::OsRng.fill_bytes(&mut secret);
        let mut body = EnvelopeBody {
            version: 1,
            manifest,
            scope,
            tuple,
            epoch,
            issuer: issuer.public_key(),
            recipient,
            ephemeral: MontgomeryPoint::mul_base_clamped(secret).to_bytes(),
            sealed: Vec::new(),
        };
        let shared = point.mul_clamped(secret).to_bytes();
        body.sealed = crate::enroll::seal(body.wrapping_key(shared)?, &body.aad()?, &key)?;
        body.validate()?;
        let signature = issuer.sign(&signing_bytes("rocky.partition-key-envelope.v1", &body)?);
        Ok(Self { body, signature })
    }
    /// Verifies delivery metadata without possessing the recipient's secret.
    pub fn verify(
        &self,
        issuer: PublicKey,
        manifest: Commitment,
        scope: ResourceScope,
        epoch: u64,
    ) -> Result<(), Error> {
        self.body.validate()?;
        if self.body.issuer != issuer
            || self.body.manifest != manifest
            || self.body.scope != scope
            || self.body.epoch != epoch
            || !issuer.verify(
                &signing_bytes("rocky.partition-key-envelope.v1", &self.body)?,
                &self.signature,
            )
        {
            return Err(denied(
                "partition envelope issuer, context or signature mismatch",
            ));
        }
        Ok(())
    }
    pub fn open(
        &self,
        recipient: &PrivateKey,
        issuer: PublicKey,
        manifest: Commitment,
        scope: ResourceScope,
        epoch: u64,
    ) -> Result<[u8; 32], Error> {
        self.verify(issuer, manifest, scope, epoch)?;
        if recipient.public_key() != self.body.recipient {
            return Err(denied("partition envelope names another recipient"));
        }
        let digest = Sha512::digest(recipient.as_bytes());
        let mut scalar = [0; 32];
        scalar.copy_from_slice(&digest[..32]);
        let shared = MontgomeryPoint(self.body.ephemeral)
            .mul_clamped(scalar)
            .to_bytes();
        crate::enroll::open(
            self.body.wrapping_key(shared)?,
            &self.body.aad()?,
            &self.body.sealed,
        )?
        .try_into()
        .map_err(|_| denied("invalid partition key length"))
    }
    pub fn recipient(&self) -> PublicKey {
        self.body.recipient
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        encode(self)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let envelope: Self = decode_bounded(bytes, 4096)?;
        envelope.body.validate()?;
        Ok(envelope)
    }
}

impl crate::CredentialChain {
    /// Verifies a node or pseudonym without discarding its partition selectors.
    pub fn verify_selectors(
        &self,
        publisher: PublicKey,
        manifest: Commitment,
        subject: PublicKey,
        policy: &crate::PolicyStatement,
        time: u64,
    ) -> Result<(Vec<ResourceGrant>, Vec<PartitionGrant>), Error> {
        let leaf = self.links(publisher, manifest, Some((policy, time)))?;
        if leaf.subject != subject
            || leaf.delegations != 0
            || !matches!(
                leaf.kind,
                rocky_core::PrincipalKind::Node | rocky_core::PrincipalKind::Pseudonym
            )
        {
            return Err(denied("session requires a node or pseudonym credential"));
        }
        Ok((leaf.grants.clone(), leaf.extended()?.partitions))
    }

    /// Verifies a node chain and resolves its paired grants on this partition.
    #[allow(clippy::too_many_arguments)]
    pub fn verify_partition(
        &self,
        publisher: PublicKey,
        manifest: Commitment,
        subject: PublicKey,
        scope: ResourceScope,
        tuple: &PartitionTuple,
        policy: &crate::PolicyStatement,
        time: u64,
    ) -> Result<Vec<ResourceGrant>, Error> {
        let leaf = self.links(publisher, manifest, Some((policy, time)))?;
        if leaf.subject != subject
            || leaf.kind != rocky_core::PrincipalKind::Node
            || leaf.delegations != 0
        {
            return Err(denied("partition session requires a node credential"));
        }
        resolve_leaf(leaf, scope, tuple)
    }
    /// Verifies an author's pseudonym and partition grant without a reader key.
    #[allow(clippy::too_many_arguments)]
    pub fn verify_partition_author(
        &self,
        publisher: PublicKey,
        manifest: Commitment,
        signer: PublicKey,
        author: u32,
        scope: ResourceScope,
        tuple: &PartitionTuple,
        policy: &crate::PolicyStatement,
        time: u64,
    ) -> Result<Vec<ResourceGrant>, Error> {
        let leaf = self.links(publisher, manifest, Some((policy, time)))?;
        if leaf.kind != rocky_core::PrincipalKind::Pseudonym
            || leaf.subject != signer
            || leaf.authors()?.and_then(|range| range.single()) != Some(author)
        {
            return Err(denied(
                "the partition author number does not belong to the signing key",
            ));
        }
        resolve_leaf(leaf, scope, tuple)
    }
    /// Delegates selected partitions, with the same attenuation checks as receipt.
    #[allow(clippy::too_many_arguments)]
    pub fn delegate_partitions(
        &self,
        holder: &PrivateKey,
        subject: PublicKey,
        kind: rocky_core::PrincipalKind,
        grants: Vec<ResourceGrant>,
        partitions: Vec<PartitionGrant>,
        terms: (u8, Option<crate::AuthorRange>),
        validity: (u64, u64, u64),
    ) -> Result<Self, Error> {
        if !matches!(
            kind,
            rocky_core::PrincipalKind::User | rocky_core::PrincipalKind::Deployment
        ) || terms.0 == 0
        {
            return Err(denied(
                "partition delegation requires a delegable principal",
            ));
        }
        self.append_partition_link(
            holder,
            subject,
            kind,
            grants,
            crate::Extended {
                authors: terms.1,
                partitions,
                ..crate::Extended::default()
            },
            terms.0,
            validity,
        )
    }
    /// Assigns one table pseudonym with selected partition rights.
    #[allow(clippy::too_many_arguments)]
    pub fn assign_partitions(
        &self,
        holder: &PrivateKey,
        key: PublicKey,
        author: u32,
        device: PublicKey,
        partitions: Vec<PartitionGrant>,
        validity: (u64, u64, u64),
    ) -> Result<Self, Error> {
        let publisher = self
            .0
            .first()
            .ok_or_else(|| denied("credential depth"))?
            .issuer;
        self.append_partition_link(
            holder,
            key,
            rocky_core::PrincipalKind::Pseudonym,
            Vec::new(),
            crate::Extended {
                authors: Some(crate::AuthorRange::new(author, author)?),
                device: Some(crate::SealedDevice::seal(publisher, key, author, device)?),
                partitions,
            },
            0,
            validity,
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn append_partition_link(
        &self,
        holder: &PrivateKey,
        subject: PublicKey,
        kind: rocky_core::PrincipalKind,
        grants: Vec<ResourceGrant>,
        extended: crate::Extended,
        delegations: u8,
        (time, ttl, skew): (u64, u64, u64),
    ) -> Result<Self, Error> {
        let root = self.0.first().ok_or_else(|| denied("credential depth"))?;
        let parent = self.links(root.issuer, root.capability.manifest, None)?;
        if parent.subject != holder.public_key() {
            return Err(denied("partition delegator holds another key"));
        }
        let mut chain = self.clone();
        chain.0.push(crate::SignedCapability::sign(
            crate::Capability {
                subject,
                kind,
                grants,
                delegations,
                not_before: time.saturating_sub(skew).max(parent.not_before),
                expires: time.saturating_add(ttl).min(parent.expires),
                extension: crate::Extension::new(extended)?,
                ..parent.clone()
            },
            holder,
        )?);
        chain.links(root.issuer, root.capability.manifest, None)?;
        Ok(chain)
    }
}
fn resolve_leaf(
    leaf: &crate::Capability,
    scope: ResourceScope,
    tuple: &PartitionTuple,
) -> Result<Vec<ResourceGrant>, Error> {
    if tuple
        .scope(scope.domain, scope.table)
        .map_err(|error| Error::Denied(error.to_string()))?
        != scope
    {
        return Err(denied("partition tuple and encryption scope differ"));
    }
    let mut grants: Vec<ResourceGrant> = leaf
        .grants
        .iter()
        .filter(|grant| grant.scope == scope)
        .cloned()
        .collect();
    for grant in leaf.extended()?.partitions {
        if let Some(grant) = grant.resolve(scope.domain, scope.table, tuple)? {
            grants.push(grant);
        }
    }
    if grants.is_empty() {
        return Err(denied("no grant for this partition"));
    }
    Ok(grants)
}
impl crate::Authority<'_> {
    /// Issues concrete and selector grants, retaining their separate row/field pairs.
    #[allow(clippy::too_many_arguments)]
    pub fn issue_partition_terms(
        &self,
        subject: PublicKey,
        grants: Vec<ResourceGrant>,
        partitions: Vec<PartitionGrant>,
        terms: (u8, Option<crate::AuthorRange>),
        epoch: u64,
        validity: (u64, u64, u64),
    ) -> Result<crate::SignedCapability, Error> {
        if partitions.is_empty() {
            return self.issue_terms(subject, grants, terms, epoch, validity);
        }
        let mut capability = self
            .issue_terms(subject, grants, terms, epoch, validity)?
            .capability;
        capability.extension = crate::Extension::new(crate::Extended {
            authors: terms.1,
            partitions,
            ..crate::Extended::default()
        })?;
        let signed = crate::SignedCapability::sign(capability, self.publisher)?;
        crate::CredentialChain(vec![signed.clone()]).links(
            self.publisher.public_key(),
            self.manifest,
            None,
        )?;
        Ok(signed)
    }
}

#[cfg(test)]
mod tests;
