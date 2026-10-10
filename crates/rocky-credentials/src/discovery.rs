//! Publisher-bound discovery material; delivered only after session admission.
use crate::{Error, PrivateKey, PublicKey, Signature, denied, signing_bytes};
use rocky_core::{Commitment, Scope};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoverySecret {
    pub scope: Scope,
    /// None identifies the domain root; Some binds a revocation statement ID.
    pub statement: Option<Commitment>,
    pub value: [u8; 32],
    signature: Signature,
}
impl DiscoverySecret {
    pub fn sign(
        scope: Scope,
        statement: Option<Commitment>,
        value: [u8; 32],
        publisher: &PrivateKey,
    ) -> Result<Self, Error> {
        let signature = publisher.sign(&signing_bytes(
            "rocky.discovery-secret.v1",
            &(scope, statement, value),
        )?);
        Ok(Self {
            scope,
            statement,
            value,
            signature,
        })
    }
    pub fn verify(&self, publisher: PublicKey, scope: Scope) -> Result<(), Error> {
        if self.scope != scope
            || !publisher.verify(
                &signing_bytes(
                    "rocky.discovery-secret.v1",
                    &(self.scope, self.statement, self.value),
                )?,
                &self.signature,
            )
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
