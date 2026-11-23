use crate::{Attestation, Capability, Error, PublicKey, Purpose, Signer, denied, signing_bytes};
use rocky_core::{Commitment, PrincipalKind};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const BINDING_VERSION: u8 = 5;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorBinding {
    version: u8,
    issuer: PublicKey,
    subject: PublicKey,
    kind: PrincipalKind,
    tables: BTreeSet<Commitment>,
    issues: bool,
    attestation: Attestation,
}
const LABEL: &str = "rocky.credentials.author-binding.v5";
fn whole_binding() -> Commitment {
    rocky_core::commitment("rocky.author-binding.whole.v1", &[])
}
impl AuthorBinding {
    pub(crate) fn sign(capability: &Capability, signer: &Signer<'_>) -> Result<Self, Error> {
        let extended = capability.extended()?;
        let tables = capability
            .grants
            .iter()
            .map(|grant| grant.scope.table)
            .chain(extended.partitions.iter().map(|grant| grant.table))
            .chain(extended.whole.map(|_| whole_binding()))
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
    fn authorizes(&self, table: Commitment, tables: &crate::WholeTables) -> bool {
        self.tables.contains(&table)
            || (self.tables.contains(&whole_binding()) && tables.covers(table))
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
    pub fn verify_historic(&self, issuer: PublicKey, node: PublicKey) -> Result<(), Error> {
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
    pub fn verify_chain(
        chain: &[Self],
        publisher: PublicKey,
        table: Commitment,
        signer: PublicKey,
        tables: &crate::WholeTables,
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
                    && !(binding.issues || binding.authorizes(table, tables)))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PrivateKey;
    use rocky_core::{ResourceGrant, ResourceScope};

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn an_author_binding_of_another_version_is_refused() {
        let (publisher, user, node) = (PrivateKey::new(), PrivateKey::new(), PrivateKey::new());
        let grant = ResourceGrant {
            scope: ResourceScope {
                domain: rocky_core::Scope {
                    mesh: rocky_core::MeshId([1; 16]),
                    app: rocky_core::AppId([2; 16]),
                    domain: rocky_core::DomainId([3; 16]),
                },
                encryption: [4; 32],
                table: [5; 32],
            },
            rows: rocky_core::RowSelection::All,
            rights: [rocky_core::Right::Insert].into(),
            fields: rocky_core::FieldSelection::All,
        };
        let identity = crate::Authority::owner(&publisher)
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
        let chain = crate::device_chain(identity, &user, &node).unwrap();
        let binding = &chain.0[1].author_binding;
        binding
            .verify_historic(user.public_key(), node.public_key())
            .unwrap();
        for version in [BINDING_VERSION - 1, BINDING_VERSION + 1] {
            let other = AuthorBinding {
                version,
                ..binding.clone()
            };
            assert!(
                other
                    .verify_historic(user.public_key(), node.public_key())
                    .is_err(),
                "version {version} is refused although its signature covers only the current one"
            );
        }
    }
}
