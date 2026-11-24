use crate::{CoordinationError, Refusal};
use p2panda_core::Signature;
use rocky_core::{AppId, ApplicationManifest};
use rocky_credentials::{IdentityProvider, PrivateKey, PublicKey, Publisher, Purpose};
use serde::{Deserialize, Serialize};

pub const MAX_APPLICATION_NAME: usize = 128;

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoordinationGrant {
    pub publisher: Publisher,
    pub application: String,
    pub tenant: CoordinatorControl,
    pub not_before: u64,
    pub expires: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedCoordinationGrant {
    pub grant: CoordinationGrant,
    signature: Signature,
}
impl SignedCoordinationGrant {
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TenantAdmission {
    pub provider: PublicKey,
    pub tenant: CoordinatorControl,
    pub not_before: u64,
    pub expires: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedTenantAdmission {
    pub admission: TenantAdmission,
    signature: Signature,
}
impl SignedTenantAdmission {
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

pub struct Tenant {
    key: PrivateKey,
    admission: Option<SignedTenantAdmission>,
}
impl Tenant {
    pub fn generate() -> Self {
        Self::from_secret(*PrivateKey::new().as_bytes())
    }
    pub fn from_secret(secret: [u8; 32]) -> Self {
        Self {
            key: PrivateKey::from_bytes(&secret),
            admission: None,
        }
    }
    pub fn secret(&self) -> [u8; 32] {
        *self.key.as_bytes()
    }
    pub fn control(&self) -> CoordinatorControl {
        CoordinatorControl(self.key.public_key())
    }
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
