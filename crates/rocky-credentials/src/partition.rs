//! Partition grants; no runtime or storage. Keys travel as group envelopes.
use crate::{Error, PublicKey, denied};
use rocky_core::{
    Commitment, FieldSelection, PartitionSelector, PartitionTuple, ResourceGrant, ResourceScope,
    Right, RowSelection, Scope,
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
    pub fields: FieldSelection,
}
impl PartitionGrant {
    pub fn validate(&self) -> Result<(), Error> {
        self.selector
            .validate()
            .map_err(|error| Error::Denied(error.to_string()))?;
        if self.rights.is_empty()
            || self.fields.named() > 256
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
            && self.fields.includes(&other.fields)
    }
    /// A concrete grant only when the selector matches the authenticated
    /// tuple. `scope` is the tuple's keyed scope: only a holder of the
    /// partition's naming secret can bind them, so the caller does.
    pub fn resolve(
        &self,
        scope: ResourceScope,
        tuple: &PartitionTuple,
    ) -> Result<Option<ResourceGrant>, Error> {
        self.validate()?;
        tuple
            .validate()
            .map_err(|error| Error::Denied(error.to_string()))?;
        Ok((self.domain == scope.domain
            && self.table == scope.table
            && self.selector.matches(tuple))
        .then(|| ResourceGrant {
            scope,
            rows: self.rows.clone(),
            rights: self.rights.clone(),
            fields: self.fields.clone(),
        }))
    }
}

impl crate::CredentialChain {
    /// Verifies a node without discarding its partition selectors.
    pub fn verify_selectors(
        &self,
        publisher: PublicKey,
        subject: PublicKey,
        revocations: &crate::RevocationSet,
        time: u64,
    ) -> Result<(Vec<ResourceGrant>, Vec<PartitionGrant>), Error> {
        let (leaf, held) = self.node(publisher, Some((revocations, time)))?;
        if leaf.subject != subject {
            return Err(denied("session requires a node credential"));
        }
        Ok((held.grants.clone(), held.extended()?.partitions))
    }

    /// Verifies a node chain and resolves its paired grants on this partition.
    pub fn verify_partition(
        &self,
        publisher: PublicKey,
        subject: PublicKey,
        scope: ResourceScope,
        tuple: &PartitionTuple,
        revocations: &crate::RevocationSet,
        time: u64,
    ) -> Result<Vec<ResourceGrant>, Error> {
        let (leaf, held) = self.node(publisher, Some((revocations, time)))?;
        if leaf.subject != subject {
            return Err(denied("partition session requires a node credential"));
        }
        resolve_leaf(held, scope, tuple)
    }
    /// Verifies a node's number and partition grant without a reader key.
    #[allow(clippy::too_many_arguments)]
    pub fn verify_partition_author(
        &self,
        publisher: PublicKey,
        signer: PublicKey,
        author: u32,
        scope: ResourceScope,
        tuple: &PartitionTuple,
        revocations: &crate::RevocationSet,
        time: u64,
    ) -> Result<Vec<ResourceGrant>, Error> {
        let (leaf, held) = self.node(publisher, Some((revocations, time)))?;
        if leaf.subject != signer
            || leaf.authors()?.and_then(|range| range.single()) != Some(author)
        {
            return Err(denied(
                "the partition author number does not belong to the signing key",
            ));
        }
        resolve_leaf(held, scope, tuple)
    }
    /// Delegates selected partitions, with the same attenuation checks as receipt.
    #[allow(clippy::too_many_arguments)]
    pub fn delegate_partitions(
        &self,
        holder: &dyn crate::IdentityProvider,
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
    #[allow(clippy::too_many_arguments)]
    fn append_partition_link(
        &self,
        holder: &dyn crate::IdentityProvider,
        subject: PublicKey,
        kind: rocky_core::PrincipalKind,
        grants: Vec<ResourceGrant>,
        extended: crate::Extended,
        delegations: u8,
        (time, ttl, skew): (u64, u64, u64),
    ) -> Result<Self, Error> {
        let root = self.0.first().ok_or_else(|| denied("credential depth"))?;
        let parent = self.links(root.issuer, None)?;
        if parent.subject != holder.public_key()? {
            return Err(denied("partition delegator holds another key"));
        }
        if parent.extended()?.issues {
            return Err(denied("an issuer grants through its authority"));
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
            },
            holder,
        )?);
        chain.links(root.issuer, None)?;
        Ok(chain)
    }
}
/// The grants `held`, a node's identity link, gives on `tuple`; `scope`
/// must be `tuple`'s keyed scope, which the caller bound.
fn resolve_leaf(
    leaf: &crate::Capability,
    scope: ResourceScope,
    tuple: &PartitionTuple,
) -> Result<Vec<ResourceGrant>, Error> {
    let mut grants: Vec<ResourceGrant> = leaf
        .grants
        .iter()
        .filter(|grant| grant.scope == scope)
        .cloned()
        .collect();
    for grant in leaf.extended()?.partitions {
        if let Some(grant) = grant.resolve(scope, tuple)? {
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
        (delegations, authors): (u8, Option<crate::AuthorRange>),
        validity: (u64, u64, u64),
    ) -> Result<crate::SignedCapability, Error> {
        self.sign_link(
            subject,
            grants,
            crate::Extended {
                authors,
                partitions,
                origin: self.origin()?,
                ..crate::Extended::default()
            },
            delegations,
            validity,
        )
    }
}

#[cfg(test)]
mod tests;

impl crate::VerifiedChain {
    pub fn partition_author(
        &self,
        subject: PublicKey,
        author: u32,
        scope: ResourceScope,
        tuple: &PartitionTuple,
        time: u64,
    ) -> Result<Vec<ResourceGrant>, Error> {
        self.selectors(subject, time)?;
        if self.leaf.authors()?.and_then(|range| range.single()) != Some(author) {
            return Err(denied(
                "the partition author number does not belong to the signing key",
            ));
        }
        resolve_leaf(&self.held, scope, tuple)
    }
    /// Whether a node may write some partition of `scope`'s table, for a
    /// verifier that cannot see which: a covered position's signed header
    /// proves only where the position lies, never what it wrote.
    pub fn unseen_partition_author(
        &self,
        subject: PublicKey,
        author: u32,
        scope: ResourceScope,
        time: u64,
    ) -> Result<(), Error> {
        let (grants, partitions) = self.selectors(subject, time)?;
        if self.leaf.authors()?.and_then(|range| range.single()) != Some(author) {
            return Err(denied(
                "the partition author number does not belong to the signing key",
            ));
        }
        let writes = |rights: &std::collections::BTreeSet<rocky_core::Right>| {
            [
                rocky_core::Right::Insert,
                rocky_core::Right::Update,
                rocky_core::Right::Delete,
            ]
            .iter()
            .any(|right| rights.contains(right))
        };
        if grants
            .iter()
            .any(|grant| grant.scope == scope && writes(&grant.rights))
            || partitions.iter().any(|grant| {
                grant.domain == scope.domain && grant.table == scope.table && writes(&grant.rights)
            })
        {
            return Ok(());
        }
        Err(denied("no write grant for a partition of this table"))
    }
}
