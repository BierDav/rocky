use super::*;
use p2panda_core::Signature;
use rocky_core::StrongRequest;
use serde::{Deserialize, Serialize};

/// Signed ingress for compiled commands. The installed author map binds the
/// authenticated node to its principal; this signature grants no data rights.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignedCommandRequest {
    node: PublicKey,
    request: Vec<u8>,
    signature: Signature,
}
impl SignedCommandRequest {
    pub fn sign(request: &StrongRequest, node: &PrivateKey) -> Result<Self, Error> {
        if request.id.node != secure_node_id(node.public_key()) {
            return Err(denied("command author key mismatch"));
        }
        let request = request.encode()?;
        let signature = node.sign(&signing_bytes("rocky.signed-command-request.v1", &request)?);
        let signed = Self {
            node: node.public_key(),
            request,
            signature,
        };
        signed.to_bytes()?;
        Ok(signed)
    }
    pub fn verify(&self) -> Result<StrongRequest, Error> {
        if !self.node.verify(
            &signing_bytes("rocky.signed-command-request.v1", &self.request)?,
            &self.signature,
        ) {
            return Err(denied("command request signature"));
        }
        let request = StrongRequest::decode(&self.request)?;
        if request.id.node != secure_node_id(self.node) {
            return Err(denied("command author key mismatch"));
        }
        Ok(request)
    }
    /// Authenticates, then returns the canonical request for transaction
    /// admission by `Replica::receive_request`.
    pub fn admitted(&self) -> Result<Vec<u8>, Error> {
        Ok(self.verify()?.encode()?)
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        let bytes = encode(self)?;
        if bytes.len() > MAX_SECURE_FRAME {
            return Err(denied("signed command request quota"));
        }
        Ok(bytes)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        decode(bytes)
    }
}
