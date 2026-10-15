//! Portable author provenance without partition selectors or extra signing keys.
use crate::{
    AuthorRange, Capability, Error, IdentityProvider, PublicKey, Purpose, Signature, denied,
    signing_bytes,
};
use rocky_core::{Commitment, PrincipalKind};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// A separately signed projection of assignment authority, revealing no tuples.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorBinding {
    version: u8,
    manifest: Commitment,
    issuer: PublicKey,
    subject: PublicKey,
    kind: PrincipalKind,
    authors: Option<AuthorRange>,
    tables: BTreeSet<Commitment>,
    /// Issuer authority: every table, whichever manifest it named.
    issues: bool,
    signature: Signature,
}
const VERSION: u8 = 2;
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
            version: VERSION,
            manifest: capability.manifest,
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
                self.manifest,
                self.issuer,
                self.subject,
                self.kind,
                self.authors,
                &self.tables,
                self.issues,
            ),
        )
    }
    /// Verifies historical assignment provenance, independently of live grants.
    pub fn verify_chain(
        chain: &[Self],
        publisher: PublicKey,
        manifest: Commitment,
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
            if binding.version != VERSION
                || (binding.manifest != manifest && !binding.issues)
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
