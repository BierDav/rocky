//! Partition grants; no runtime or storage. Keys travel as group envelopes.
use crate::{Error, PrivateKey, PublicKey, denied};
use rocky_core::{
    Commitment, PartitionSelector, PartitionTuple, ResourceGrant, ResourceScope, Right,
    RowSelection, Scope,
};
use serde::{Deserialize, Serialize};
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

impl crate::CredentialChain {
    /// Verifies a node or pseudonym without discarding its partition selectors.
    pub fn verify_selectors(
        &self,
        publisher: PublicKey,
        manifest: Commitment,
        subject: PublicKey,
        revocations: &crate::RevocationSet,
        time: u64,
    ) -> Result<(Vec<ResourceGrant>, Vec<PartitionGrant>), Error> {
        let leaf = self.links(publisher, manifest, Some((revocations, time)))?;
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
        revocations: &crate::RevocationSet,
        time: u64,
    ) -> Result<Vec<ResourceGrant>, Error> {
        let leaf = self.links(publisher, manifest, Some((revocations, time)))?;
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
        revocations: &crate::RevocationSet,
        time: u64,
    ) -> Result<Vec<ResourceGrant>, Error> {
        let leaf = self.links(publisher, manifest, Some((revocations, time)))?;
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
        validity: (u64, u64, u64),
    ) -> Result<crate::SignedCapability, Error> {
        if partitions.is_empty() {
            return self.issue_terms(subject, grants, terms, validity);
        }
        let mut capability = self
            .issue_terms(subject, grants, terms, validity)?
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
