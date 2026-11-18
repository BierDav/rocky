use crate::{CredentialChain, Endorsement, Error, PublicKey, RevocationSet, denied};
use rocky_core::{Commitment, Scope};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoverySecret {
    pub scope: Scope,
    pub statement: Option<Commitment>,
    pub value: [u8; 32],
    endorsement: Endorsement,
}
const SECRET: &str = "rocky.discovery-secret.v2";
impl DiscoverySecret {
    pub fn sign(
        scope: Scope,
        statement: Option<Commitment>,
        value: [u8; 32],
        (signer, chain): (crate::Signer<'_>, &CredentialChain),
    ) -> Result<Self, Error> {
        Ok(Self {
            scope,
            statement,
            value,
            endorsement: Endorsement::sign(signer, chain, SECRET, &(scope, statement, value))?,
        })
    }
    pub fn verify_historic(&self, publisher: PublicKey, scope: Scope) -> Result<(), Error> {
        self.check(publisher, scope, None)
    }
    pub fn verify_root(&self, publisher: PublicKey, scope: Scope) -> Result<(), Error> {
        self.check(publisher, scope, None)?;
        let chain = &self.endorsement.chain;
        let signed_by_root_or_trusted =
            chain.0.is_empty() || (chain.0.len() == 1 && chain.trusted_subject()?.is_some());
        if self.statement.is_some() || !signed_by_root_or_trusted {
            return Err(denied(
                "a discovery root comes from the root key or a trusted identity",
            ));
        }
        Ok(())
    }
    pub fn verify_unrevoked(
        &self,
        publisher: PublicKey,
        scope: Scope,
        revocations: &RevocationSet,
    ) -> Result<(), Error> {
        self.check(publisher, scope, Some(revocations))
    }
    fn check(
        &self,
        publisher: PublicKey,
        scope: Scope,
        revocations: Option<&RevocationSet>,
    ) -> Result<(), Error> {
        let message = (self.scope, self.statement, self.value);
        let verified = match revocations {
            None => self
                .endorsement
                .verify_historic(publisher, SECRET, &message),
            Some(revocations) => {
                self.endorsement
                    .verify_unrevoked(publisher, SECRET, &message, revocations)
            }
        };
        if self.scope != scope || verified.is_err() {
            return Err(denied("invalid discovery secret"));
        }
        Ok(())
    }
}

pub fn seal_discovery(key: Commitment, bytes: &[u8]) -> Result<Vec<u8>, Error> {
    crate::enroll::seal(key, b"rocky.discovery-session.v1", bytes)
}
pub fn open_discovery(key: Commitment, bytes: &[u8]) -> Result<Vec<u8>, Error> {
    crate::enroll::open(key, b"rocky.discovery-session.v1", bytes)
}
