//! Publisher-bound discovery material; delivered only after session admission.
use crate::{CredentialChain, Endorsement, Error, IdentityProvider, PublicKey, denied};
use rocky_core::{Commitment, Scope};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoverySecret {
    pub scope: Scope,
    /// None identifies the domain root; Some binds a revocation statement ID.
    pub statement: Option<Commitment>,
    pub value: [u8; 32],
    endorsement: Endorsement,
}
const SECRET: &str = "rocky.discovery-secret.v2";
impl DiscoverySecret {
    /// The owner, or an issuer through `chain`, signs.
    pub fn sign(
        scope: Scope,
        statement: Option<Commitment>,
        value: [u8; 32],
        (signer, chain): (&dyn IdentityProvider, &CredentialChain),
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
                .verify(publisher, SECRET, &(self.scope, self.statement, self.value))
                .is_err()
        {
            return Err(denied("invalid discovery secret"));
        }
        Ok(())
    }
}

/// Encrypts session-only discovery material under a session-bound exchange key.
pub fn seal_discovery(key: Commitment, bytes: &[u8]) -> Result<Vec<u8>, Error> {
    crate::enroll::seal(key, b"rocky.discovery-session.v1", bytes)
}
pub fn open_discovery(key: Commitment, bytes: &[u8]) -> Result<Vec<u8>, Error> {
    crate::enroll::open(key, b"rocky.discovery-session.v1", bytes)
}
