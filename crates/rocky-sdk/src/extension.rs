use crate::{Failure, Session, request::Request};
use rocky_credentials::{CredentialChain, PublicKey, RevocationSet, Signature};

#[derive(Clone, Debug)]
pub struct DeviceCredential {
    pub scope: crate::Scope,
    pub publisher: PublicKey,
    pub identity: PublicKey,
    pub node: PublicKey,
    pub chain: CredentialChain,
    pub revocations: RevocationSet,
}

impl Session {
    pub async fn device_credential(&self) -> Result<DeviceCredential, Failure> {
        self.send(Request::DeviceCredential).await
    }
    pub async fn sign_extension(&self, label: &str, message: &[u8]) -> Result<Signature, Failure> {
        let signing = rocky_credentials::extension_message(label, message)?;
        self.send(|reply| Request::SignExtension(signing, reply))
            .await
    }
}
