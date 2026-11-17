use crate::{CredentialChain, Endorsement, Error, PublicKey, denied};
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
    pub fn verify(&self, publisher: PublicKey, scope: Scope) -> Result<(), Error> {
        if self.scope != scope
            || self
                .endorsement
                .verify_historic(publisher, SECRET, &(self.scope, self.statement, self.value))
                .is_err()
        {
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
