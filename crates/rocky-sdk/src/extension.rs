use crate::{Database, request::Request};
use rocky_credentials::{CredentialChain, PublicKey, RevocationSet, Signature};

/// This device's credentials, for add-on services such as rocky-coordinator to verify.
#[derive(Clone, Debug)]
pub struct DeviceCredential {
    /// The database.
    pub scope: crate::Scope,
    /// The application name.
    pub application: String,
    /// The owner's root key.
    pub publisher: PublicKey,
    /// The person key.
    pub identity: PublicKey,
    /// The device key.
    pub node: PublicKey,
    /// The chain that proves the device's membership.
    pub chain: CredentialChain,
    /// The revocations this device holds.
    pub revocations: RevocationSet,
}

impl Database {
    /// This device's credentials.
    pub async fn device_credential(&self) -> Result<DeviceCredential, crate::Error> {
        self.send(Request::DeviceCredential).await
    }
    /// Signs `message` with the device key under an add-on's `label`, which keeps the signature
    /// from being valid for Rocky itself.
    pub async fn sign_extension(
        &self,
        label: &str,
        message: &[u8],
    ) -> Result<Signature, crate::Error> {
        let signing = rocky_credentials::extension_message(label, message)
            .map_err(|error| crate::Error::failed(error.into()))?;
        self.send(|reply| Request::SignExtension(signing, reply))
            .await
    }
}
