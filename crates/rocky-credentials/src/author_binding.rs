//! Portable author provenance without partition selectors or extra signing keys.
use crate::{Attestation, Capability, Error, PublicKey, Purpose, Signer, denied, signing_bytes};
use rocky_core::{Commitment, PrincipalKind};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// The current author binding format; others are refused.
const BINDING_VERSION: u8 = 4;

/// A separately signed projection of assignment authority, revealing no tuples.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorBinding {
    version: u8,
    issuer: PublicKey,
    subject: PublicKey,
    kind: PrincipalKind,
    tables: BTreeSet<Commitment>,
    /// Issuer authority: every table.
    issues: bool,
    attestation: Attestation,
}
const LABEL: &str = "rocky.credentials.author-binding.v4";
impl AuthorBinding {
    pub(crate) fn sign(capability: &Capability, signer: &Signer<'_>) -> Result<Self, Error> {
        let extended = capability.extended()?;
        let tables = capability
            .grants
            .iter()
            .map(|grant| {
                crate::families::binding_table(
                    grant.scope.table,
                    extended.families,
                    &grant.rows,
                    &grant.fields,
                )
            })
            .chain(extended.partitions.iter().map(|grant| {
                crate::families::binding_table(
                    grant.table,
                    extended.families,
                    &grant.rows,
                    &grant.fields,
                )
            }))
            .collect();
        let issuer = signer.principal()?;
        let bytes = Self::body(
            issuer,
            capability.subject,
            capability.kind,
            &tables,
            extended.issues,
        )?;
        Ok(Self {
            version: BINDING_VERSION,
            issuer,
            subject: capability.subject,
            kind: capability.kind,
            tables,
            issues: extended.issues,
            attestation: signer.attest(Purpose::Capability, &bytes)?,
        })
    }
    fn body(
        issuer: PublicKey,
        subject: PublicKey,
        kind: PrincipalKind,
        tables: &BTreeSet<Commitment>,
        issues: bool,
    ) -> Result<Vec<u8>, Error> {
        signing_bytes(
            LABEL,
            &(BINDING_VERSION, issuer, subject, kind, tables, issues),
        )
    }
    fn bytes(&self) -> Result<Vec<u8>, Error> {
        Self::body(
            self.issuer,
            self.subject,
            self.kind,
            &self.tables,
            self.issues,
        )
    }
    /// Certifies a node writer by one exact identity, without revealing
    /// partition selectors. Row admission still checks live grants.
    pub fn verify_node_issuer(&self, issuer: PublicKey, node: PublicKey) -> Result<(), Error> {
        if self.version != BINDING_VERSION
            || self.issuer != issuer
            || self.subject != node
            || self.kind != PrincipalKind::Node
            || self.issues
            || self.attestation.certificate().is_some()
            || self.attestation.verify(issuer, &self.bytes()?).is_err()
        {
            return Err(denied("migration writer assignment"));
        }
        Ok(())
    }
    /// Verifies historical writer provenance, independently of live grants:
    /// the chain ends at the node `signer`, whose key is the writer.
    pub fn verify_chain(
        chain: &[Self],
        publisher: PublicKey,
        table: Commitment,
        signer: PublicKey,
    ) -> Result<(), Error> {
        Self::verify_chain_with_families(
            chain,
            publisher,
            table,
            signer,
            &crate::GrantFamilies::default(),
        )
    }
    pub fn verify_chain_with_families(
        chain: &[Self],
        publisher: PublicKey,
        table: Commitment,
        signer: PublicKey,
        families: &crate::GrantFamilies,
    ) -> Result<(), Error> {
        if chain.is_empty() || chain.len() > crate::MAX_CREDENTIAL_DEPTH {
            return Err(denied("author binding chain quota"));
        }
        let mut issuer = publisher;
        let mut parent_hands_on_issuance = true;
        for (index, binding) in chain.iter().enumerate() {
            if binding.version != BINDING_VERSION
                || binding.issuer != issuer
                || (binding.kind != PrincipalKind::Node
                    && !(binding.issues || families.authorizes_binding(&binding.tables, table)))
                || (binding.issues
                    && (binding.kind != PrincipalKind::User || !parent_hands_on_issuance))
                || binding
                    .attestation
                    .verify(issuer, &binding.bytes()?)
                    .is_err()
                || (binding.kind == PrincipalKind::Node
                    && binding.attestation.certificate().is_some())
                || (index + 1 < chain.len() && binding.kind == PrincipalKind::Node)
            {
                return Err(denied("invalid portable author binding"));
            }
            issuer = binding.subject;
            parent_hands_on_issuance = binding.issues;
        }
        let leaf = &chain[chain.len() - 1];
        if leaf.kind != PrincipalKind::Node || issuer != signer {
            return Err(denied("creation proof author does not own identity"));
        }
        Ok(())
    }
}
