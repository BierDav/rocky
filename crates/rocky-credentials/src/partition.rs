use crate::{Error, PublicKey, denied};
use rocky_core::{
    Commitment, FieldSelection, PartitionSelector, PartitionTuple, ResourceGrant, ResourceScope,
    Right, RowSelection, Scope,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

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

#[derive(Clone, Copy, Debug)]
pub struct PartitionContext<'a> {
    pub scope: ResourceScope,
    pub tuple: &'a PartitionTuple,
}

impl crate::CredentialChain {
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
    pub fn verify_partition_author(
        &self,
        publisher: PublicKey,
        signer: PublicKey,
        partition: PartitionContext<'_>,
        revocations: &crate::RevocationSet,
        time: u64,
    ) -> Result<Vec<ResourceGrant>, Error> {
        let PartitionContext { scope, tuple } = partition;
        let (leaf, held) = self.node(publisher, Some((revocations, time)))?;
        if leaf.subject != signer {
            return Err(denied("the partition writer is not the signing node"));
        }
        resolve_leaf(held, scope, tuple)
    }
    pub fn delegate_partitions<'s>(
        &self,
        holder: impl Into<crate::Signer<'s>>,
        request: crate::Delegation,
    ) -> Result<Self, Error> {
        if !matches!(
            request.kind,
            rocky_core::PrincipalKind::User | rocky_core::PrincipalKind::Deployment
        ) || request.terms.delegations == 0
        {
            return Err(denied(
                "partition delegation requires a delegable principal",
            ));
        }
        self.append_partition_link(holder.into(), request)
    }
    fn append_partition_link(
        &self,
        holder: crate::Signer<'_>,
        request: crate::Delegation,
    ) -> Result<Self, Error> {
        let crate::Delegation {
            subject,
            kind,
            grants,
            partitions,
            terms,
            validity,
        } = request;
        let crate::Validity {
            issued_at: time,
            lifetime,
            clock_skew,
        } = validity;
        let ttl = lifetime.as_secs();
        let skew = clock_skew.as_secs();
        let delegations = terms.delegations;
        let extended = crate::Extended {
            partitions,
            ..crate::Extended::default()
        };
        let root = self.0.first().ok_or_else(|| denied("credential depth"))?;
        let parent = self.links(root.issuer, None)?;
        if parent.subject != holder.principal()? {
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
    pub fn issue_partition_terms(
        &self,
        subject: PublicKey,
        grants: Vec<ResourceGrant>,
        partitions: Vec<PartitionGrant>,
        terms: crate::DelegationTerms,
        validity: crate::Validity,
    ) -> Result<crate::SignedCapability, Error> {
        self.issue_table_terms(subject, grants, partitions, terms, validity, false)
    }
    pub fn issue_family_terms(
        &self,
        subject: PublicKey,
        grants: Vec<ResourceGrant>,
        partitions: Vec<PartitionGrant>,
        terms: crate::DelegationTerms,
        validity: crate::Validity,
    ) -> Result<crate::SignedCapability, Error> {
        self.issue_table_terms(subject, grants, partitions, terms, validity, true)
    }
    fn issue_table_terms(
        &self,
        subject: PublicKey,
        grants: Vec<ResourceGrant>,
        partitions: Vec<PartitionGrant>,
        crate::DelegationTerms { delegations }: crate::DelegationTerms,
        validity: crate::Validity,
        families: bool,
    ) -> Result<crate::SignedCapability, Error> {
        self.sign_link(
            subject,
            grants,
            crate::Extended {
                partitions,
                families,
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
        scope: ResourceScope,
        tuple: &PartitionTuple,
        revocations: &crate::RevocationSet,
        time: u64,
    ) -> Result<Vec<ResourceGrant>, Error> {
        self.partition(subject, scope, tuple, revocations, time)
    }
    pub fn unseen_partition_author(
        &self,
        subject: PublicKey,
        scope: ResourceScope,
        revocations: &crate::RevocationSet,
        time: u64,
    ) -> Result<(), Error> {
        self.valid_node(subject, revocations, time)?;
        let (grants, partitions) = self.grants_for(scope.table)?;
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
