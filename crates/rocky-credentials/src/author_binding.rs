//! Portable author provenance without partition selectors or device bindings.
use crate::{
    AuthorRange, Capability, Error, PrivateKey, PublicKey, Signature, denied, signing_bytes,
};
use rocky_core::{Commitment, PrincipalKind};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// A separately signed projection of assignment authority, revealing no tuples.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorBinding {
    version: u8,
    manifest: Commitment,
    issuer: PublicKey,
    subject: PublicKey,
    kind: PrincipalKind,
    authors: Option<AuthorRange>,
    tables: BTreeSet<Commitment>,
    signature: Signature,
}
impl AuthorBinding {
    pub(crate) fn sign(capability: &Capability, issuer: &PrivateKey) -> Result<Self, Error> {
        let extended = capability.extended()?;
        let tables = capability
            .grants
            .iter()
            .map(|grant| grant.scope.table)
            .chain(extended.partitions.iter().map(|grant| grant.table))
            .collect();
        let mut binding = Self {
            version: 1,
            manifest: capability.manifest,
            issuer: issuer.public_key(),
            subject: capability.subject,
            kind: capability.kind,
            authors: extended.authors,
            tables,
            signature: issuer.sign(b""),
        };
        binding.signature = issuer.sign(&binding.bytes()?);
        Ok(binding)
    }
    fn bytes(&self) -> Result<Vec<u8>, Error> {
        signing_bytes(
            "rocky.credentials.author-binding.v1",
            &(
                self.version,
                self.manifest,
                self.issuer,
                self.subject,
                self.kind,
                self.authors,
                &self.tables,
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
        for (index, binding) in chain.iter().enumerate() {
            let assigned = binding
                .authors
                .ok_or_else(|| denied("author binding lacks range"))?;
            AuthorRange::new(assigned.first, assigned.last)?;
            if binding.version != 1
                || binding.manifest != manifest
                || binding.issuer != issuer
                || !range.includes(&assigned)
                || !binding.tables.contains(&table)
                || !issuer.verify(&binding.bytes()?, &binding.signature)
                || (index + 1 < chain.len()
                    && matches!(binding.kind, PrincipalKind::Node | PrincipalKind::Pseudonym))
            {
                return Err(denied("invalid portable author binding"));
            }
            issuer = binding.subject;
            range = assigned;
        }
        let leaf = &chain[chain.len() - 1];
        if leaf.kind != PrincipalKind::Pseudonym
            || issuer != signer
            || range.single() != Some(author)
        {
            return Err(denied("creation proof author does not own identity"));
        }
        Ok(())
    }
}
