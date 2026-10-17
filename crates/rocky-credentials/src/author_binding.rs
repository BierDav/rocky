//! Portable author provenance without partition selectors or extra signing keys.
use crate::{
    AuthorRange, Capability, Error, IdentityProvider, PublicKey, Purpose, Signature, denied,
    signing_bytes,
};
use rocky_core::{Commitment, PrincipalKind};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// The current author binding format; others are refused.
const BINDING_VERSION: u8 = 2;

/// A separately signed projection of assignment authority, revealing no tuples.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorBinding {
    version: u8,
    issuer: PublicKey,
    subject: PublicKey,
    kind: PrincipalKind,
    authors: Option<AuthorRange>,
    tables: BTreeSet<Commitment>,
    /// Issuer authority: every table.
    issues: bool,
    signature: Signature,
}
const LABEL: &str = "rocky.credentials.author-binding.v2";
impl AuthorBinding {
    pub(crate) fn sign(
        capability: &Capability,
        issuer: &dyn IdentityProvider,
    ) -> Result<Self, Error> {
        let extended = capability.extended()?;
        let tables = capability
            .grants
            .iter()
            .map(|grant| grant.scope.table)
            .chain(extended.partitions.iter().map(|grant| grant.table))
            .collect();
        let mut binding = Self {
            version: BINDING_VERSION,
            issuer: issuer.public_key()?,
            subject: capability.subject,
            kind: capability.kind,
            authors: extended.authors,
            tables,
            issues: extended.issues,
            // Replaced below: the signature covers every other field.
            signature: crate::PrivateKey::new().sign(b""),
        };
        binding.signature = issuer.sign(Purpose::Capability, &binding.bytes()?)?;
        Ok(binding)
    }
    fn bytes(&self) -> Result<Vec<u8>, Error> {
        signing_bytes(
            LABEL,
            &(
                self.version,
                self.issuer,
                self.subject,
                self.kind,
                self.authors,
                &self.tables,
                self.issues,
            ),
        )
    }
    /// Certifies a node's author assignment by one exact identity, without
    /// revealing partition selectors. Row admission still checks live grants.
    pub fn verify_node_issuer(
        &self,
        issuer: PublicKey,
        node: PublicKey,
        author: u32,
    ) -> Result<(), Error> {
        if self.version != BINDING_VERSION
            || self.issuer != issuer
            || self.subject != node
            || self.kind != PrincipalKind::Node
            || self.issues
            || self.authors.and_then(|range| range.single()) != Some(author)
            || !issuer.verify(&self.bytes()?, &self.signature)
        {
            return Err(denied("migration writer assignment"));
        }
        Ok(())
    }
    /// Verifies historical assignment provenance, independently of live grants.
    pub fn verify_chain(
        chain: &[Self],
        publisher: PublicKey,
        table: Commitment,
        author: u32,
        signer: PublicKey,
    ) -> Result<(), Error> {
        if chain.is_empty() || chain.len() > crate::MAX_CREDENTIAL_DEPTH {
            return Err(denied("author binding chain quota"));
        }
        let mut issuer = publisher;
        let mut range = AuthorRange::ALL;
        // Only the publisher or an issuer hands on issuer authority.
        let mut authority = true;
        for (index, binding) in chain.iter().enumerate() {
            let assigned = binding
                .authors
                .ok_or_else(|| denied("author binding lacks range"))?;
            AuthorRange::new(assigned.first, assigned.last)?;
            if binding.version != BINDING_VERSION
                || binding.issuer != issuer
                || !range.includes(&assigned)
                || !(binding.issues || binding.tables.contains(&table))
                || (binding.issues && (binding.kind != PrincipalKind::User || !authority))
                || !issuer.verify(&binding.bytes()?, &binding.signature)
                || (index + 1 < chain.len() && binding.kind == PrincipalKind::Node)
            {
                return Err(denied("invalid portable author binding"));
            }
            issuer = binding.subject;
            // Issuer authority hands out any range, as the publisher does.
            range = if binding.issues {
                AuthorRange::ALL
            } else {
                assigned
            };
            authority = binding.issues;
        }
        let leaf = &chain[chain.len() - 1];
        if leaf.kind != PrincipalKind::Node || issuer != signer || range.single() != Some(author) {
            return Err(denied("creation proof author does not own identity"));
        }
        Ok(())
    }
}
