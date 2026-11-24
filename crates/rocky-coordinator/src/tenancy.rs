use crate::{CoordinationError, Refusal};
use p2panda_core::Signature;
use rocky_core::{AppId, ApplicationManifest};
use rocky_credentials::{IdentityProvider, PrivateKey, PublicKey, Publisher, Purpose};
use serde::{Deserialize, Serialize};

/// The longest application name in a grant, in bytes.
pub const MAX_APPLICATION_NAME: usize = 128;

/// The public key that identifies a tenant to the coordinator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CoordinatorControl(pub PublicKey);

fn signing_bytes<T: Serialize>(label: &str, value: &T) -> Result<Vec<u8>, CoordinationError> {
    rocky_credentials::signing_bytes(label, value).map_err(|_| CoordinationError::Corrupt)
}

fn current(
    not_before: u64,
    expires: u64,
    time: u64,
    outside: Refusal,
) -> Result<(), CoordinationError> {
    if not_before >= expires || not_before > time || expires <= time {
        return Err(CoordinationError::Denied(outside));
    }
    Ok(())
}

/// A database root key's permission for a tenant to register the database.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoordinationGrant {
    /// The database's root key.
    pub publisher: Publisher,
    /// The application name in the database's manifest.
    pub application: String,
    /// The tenant allowed to register.
    pub tenant: CoordinatorControl,
    /// Unix seconds from which the grant is valid.
    pub not_before: u64,
    /// Unix seconds at which the grant expires.
    pub expires: u64,
}
/// A grant signed by the database's root key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedCoordinationGrant {
    /// The signed grant.
    pub grant: CoordinationGrant,
    signature: Signature,
}
impl SignedCoordinationGrant {
    /// Signs with the root key, refusing a key that is not the grant's publisher.
    pub fn sign(
        grant: CoordinationGrant,
        owner: &dyn IdentityProvider,
    ) -> Result<Self, CoordinationError> {
        if owner.public_key().ok() != Some(grant.publisher.0) {
            return Err(CoordinationError::Denied(Refusal::InvalidGrant));
        }
        Ok(Self {
            signature: owner
                .sign(
                    Purpose::Statement,
                    &signing_bytes("rocky.coordination-grant.v1", &grant)?,
                )
                .map_err(|_| CoordinationError::Denied(Refusal::InvalidGrant))?,
            grant,
        })
    }
    /// Checks the signature and validity at `time`, returning the application id.
    pub fn verify(&self, time: u64) -> Result<AppId, CoordinationError> {
        let grant = &self.grant;
        if grant.application.is_empty()
            || grant.application.len() > MAX_APPLICATION_NAME
            || !grant.publisher.0.verify(
                &signing_bytes("rocky.coordination-grant.v1", grant)?,
                &self.signature,
            )
        {
            return Err(CoordinationError::Denied(Refusal::InvalidGrant));
        }
        current(grant.not_before, grant.expires, time, Refusal::InvalidGrant)?;
        Ok(ApplicationManifest::app_id(
            *grant.publisher.0.as_bytes(),
            &grant.application,
        ))
    }
}

/// A coordinator provider's permission for a tenant to register on a public service.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TenantAdmission {
    /// The provider's key, which the coordinator's genesis names.
    pub provider: PublicKey,
    /// The admitted tenant.
    pub tenant: CoordinatorControl,
    /// Unix seconds from which the admission is valid.
    pub not_before: u64,
    /// Unix seconds at which the admission expires.
    pub expires: u64,
}
/// An admission signed by the provider.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedTenantAdmission {
    /// The signed admission.
    pub admission: TenantAdmission,
    signature: Signature,
}
impl SignedTenantAdmission {
    /// Signs with the provider's key, refusing a key that is not the admission's provider.
    pub fn sign(
        admission: TenantAdmission,
        provider: &PrivateKey,
    ) -> Result<Self, CoordinationError> {
        if admission.provider != provider.public_key() {
            return Err(CoordinationError::Denied(Refusal::NotAdmitted));
        }
        Ok(Self {
            signature: provider.sign(&signing_bytes("rocky.tenant-admission.v1", &admission)?),
            admission,
        })
    }
    /// Checks the signature, provider and validity at `time`, returning the tenant.
    pub fn verify(
        &self,
        provider: PublicKey,
        time: u64,
    ) -> Result<CoordinatorControl, CoordinationError> {
        let admission = &self.admission;
        if admission.provider != provider
            || !provider.verify(
                &signing_bytes("rocky.tenant-admission.v1", admission)?,
                &self.signature,
            )
        {
            return Err(CoordinationError::Denied(Refusal::NotAdmitted));
        }
        current(
            admission.not_before,
            admission.expires,
            time,
            Refusal::NotAdmitted,
        )?;
        Ok(admission.tenant)
    }
}

/// A coordinator account; store its `secret` to renew registrations later.
pub struct Tenant {
    key: PrivateKey,
    admission: Option<SignedTenantAdmission>,
}
impl Tenant {
    /// A new random tenant.
    pub fn generate() -> Self {
        Self::from_secret(*PrivateKey::new().as_bytes())
    }
    /// The tenant a stored secret restores.
    pub fn from_secret(secret: [u8; 32]) -> Self {
        Self {
            key: PrivateKey::from_bytes(&secret),
            admission: None,
        }
    }
    /// The 32-byte secret to store; anyone holding it controls the tenant's registrations.
    pub fn secret(&self) -> [u8; 32] {
        *self.key.as_bytes()
    }
    /// The tenant's public identity.
    pub fn control(&self) -> CoordinatorControl {
        CoordinatorControl(self.key.public_key())
    }
    /// Attaches a provider's admission, refusing one issued to another tenant.
    pub fn admitted(self, admission: SignedTenantAdmission) -> Result<Self, CoordinationError> {
        if admission.admission.tenant != self.control() {
            return Err(CoordinationError::Denied(Refusal::NotAdmitted));
        }
        Ok(Self {
            admission: Some(admission),
            ..self
        })
    }
    #[cfg(feature = "database")]
    pub(crate) fn key(&self) -> &PrivateKey {
        &self.key
    }
    /// The attached admission, if any.
    pub fn admission(&self) -> Option<&SignedTenantAdmission> {
        self.admission.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn grants_bind_their_signer_application_and_window() {
        let (publisher, tenant) = (PrivateKey::new(), PrivateKey::new());
        let grant = CoordinationGrant {
            publisher: Publisher(publisher.public_key()),
            application: "notes".into(),
            tenant: CoordinatorControl(tenant.public_key()),
            not_before: 10,
            expires: 20,
        };
        let signed = SignedCoordinationGrant::sign(grant.clone(), &publisher).unwrap();
        assert_eq!(
            signed.verify(15).unwrap(),
            ApplicationManifest::app_id(*publisher.public_key().as_bytes(), "notes")
        );
        assert!(signed.verify(20).is_err());
        assert!(signed.verify(9).is_err());
        assert!(
            SignedCoordinationGrant::sign(grant, &tenant).is_err(),
            "only the publisher signs a coordination grant"
        );
        let mut renamed = signed.clone();
        renamed.grant.application = "other".into();
        assert!(renamed.verify(15).is_err());

        let provider = PrivateKey::new();
        let admission = SignedTenantAdmission::sign(
            TenantAdmission {
                provider: provider.public_key(),
                tenant: CoordinatorControl(tenant.public_key()),
                not_before: 10,
                expires: 20,
            },
            &provider,
        )
        .unwrap();
        assert_eq!(
            admission.verify(provider.public_key(), 15).unwrap().0,
            tenant.public_key()
        );
        assert!(admission.verify(publisher.public_key(), 15).is_err());
        assert!(admission.verify(provider.public_key(), 25).is_err());
    }
}
