//! Control tokens of a coordination service. They delegate coordination
//! metadata only: no row, key, command or enrollment right follows from
//! either. See `docs/runtime/public-coordination.md`.
use crate::{
    CoordinatorControl, Error, IdentityProvider, PrivateKey, PublicKey, Publisher, Purpose, denied,
    signing_bytes,
};
use p2panda_core::Signature;
use rocky_core::{AppId, ApplicationManifest};
use serde::{Deserialize, Serialize};

/// Longest application name a grant carries.
pub const MAX_APPLICATION_NAME: usize = 128;

fn current(not_before: u64, expires: u64, time: u64) -> Result<(), Error> {
    if not_before >= expires || not_before > time || expires <= time {
        return Err(denied("control token is not current"));
    }
    Ok(())
}

/// The owner, whose identity key is the publisher key, lets a tenant
/// control key register and control the coordinator domains of one
/// application. The application id is derived
/// from the publisher and name, so nobody else can claim it.
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
    /// The owner's identity signs.
    pub fn sign(grant: CoordinationGrant, owner: &dyn IdentityProvider) -> Result<Self, Error> {
        if grant.publisher.0 != owner.public_key()? {
            return Err(denied("grant publisher mismatch"));
        }
        Ok(Self {
            signature: owner.sign(
                Purpose::Statement,
                &signing_bytes("rocky.coordination-grant.v1", &grant)?,
            )?,
            grant,
        })
    }
    /// The application the tenant may coordinate at `time`.
    pub fn verify(&self, time: u64) -> Result<AppId, Error> {
        let grant = &self.grant;
        if grant.application.is_empty()
            || grant.application.len() > MAX_APPLICATION_NAME
            || !grant.publisher.0.verify(
                &signing_bytes("rocky.coordination-grant.v1", grant)?,
                &self.signature,
            )
        {
            return Err(denied("invalid coordination grant"));
        }
        current(grant.not_before, grant.expires, time)?;
        Ok(ApplicationManifest::app_id(
            *grant.publisher.0.as_bytes(),
            &grant.application,
        ))
    }
}

/// The provider admits a tenant control key to its service. It is the
/// tenant's account: quotas are owned by the key, whatever login or billing
/// led the provider to sign it.
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
    pub fn sign(admission: TenantAdmission, provider: &PrivateKey) -> Result<Self, Error> {
        if admission.provider != provider.public_key() {
            return Err(denied("admission provider mismatch"));
        }
        Ok(Self {
            signature: provider.sign(&signing_bytes("rocky.tenant-admission.v1", &admission)?),
            admission,
        })
    }
    pub fn verify(&self, provider: PublicKey, time: u64) -> Result<CoordinatorControl, Error> {
        let admission = &self.admission;
        if admission.provider != provider
            || !provider.verify(
                &signing_bytes("rocky.tenant-admission.v1", admission)?,
                &self.signature,
            )
        {
            return Err(denied("invalid tenant admission"));
        }
        current(admission.not_before, admission.expires, time)?;
        Ok(admission.tenant)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
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
        // Only the publisher signs, and a changed name breaks the signature.
        assert!(SignedCoordinationGrant::sign(grant, &tenant).is_err());
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
