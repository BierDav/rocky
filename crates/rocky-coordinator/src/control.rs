use crate::tenancy::{SignedCoordinationGrant, SignedTenantAdmission};
use crate::{
    certified::{ControlCertificate, DomainControl, TrustBundle},
    service::NetworkCoordinator,
};
use rocky_core::{Commitment, Scope, net::Transport};
use rocky_credentials::{PrivateKey, PublicKey};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::Mutex;

type Failure = Box<dyn std::error::Error + Send + Sync>;

/// A registration or renewal an operator publishes for a database.
pub struct ControlRequest {
    /// The tenant key that signs the control certificate.
    pub tenant: PrivateKey,
    /// The 1 to 32 identity keys whose devices may call the coordinator.
    pub trusted: Vec<PublicKey>,
    /// Seconds the certificate is valid.
    pub lifetime: u64,
    /// The request id, which keeps a retry from applying twice.
    pub id: Commitment,
    /// A first registration or a later revision.
    pub revision: ControlRevision,
}
/// Whether a control certificate registers a database or updates it.
pub enum ControlRevision {
    /// The first registration, revision 1.
    Register {
        /// The root key's grant to this tenant.
        grant: Option<Box<SignedCoordinationGrant>>,
        /// The provider's admission of this tenant, if the service needs one.
        admission: Option<Box<SignedTenantAdmission>>,
    },
    /// A later revision, above 1.
    Update(u64),
}
/// Proof of an accepted registration or renewal; keep it to renew.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlReceipt {
    /// The tenant that signed it.
    pub tenant: PublicKey,
    /// Its control revision.
    pub revision: u64,
    /// The accepted certificate's reference.
    pub control: Commitment,
    /// Unix seconds when it expires; renew before then.
    pub expires: u64,
}
/// The database a control request is for.
pub struct Domain {
    /// The database's scope.
    pub scope: Scope,
    /// The application name in the database's manifest.
    pub application: String,
    /// The database's root key.
    pub publisher: PublicKey,
}
#[derive(serde::Serialize, serde::Deserialize)]
struct Signed {
    tenant: [u8; 32],
    id: Commitment,
    inputs: Commitment,
    time: u64,
}

/// An operator's client that registers, renews and closes databases.
pub struct ControlClient {
    bundle: TrustBundle,
    transport: Arc<dyn Transport>,
    directory: PathBuf,
    lock: Mutex<()>,
}
impl ControlClient {
    /// A client for the coordinator a trust bundle names, keeping its state in `directory`.
    pub fn new(bundle: TrustBundle, transport: Arc<dyn Transport>, directory: PathBuf) -> Self {
        Self {
            bundle,
            transport,
            directory,
            lock: Mutex::new(()),
        }
    }
    fn client(&self, key: PrivateKey) -> Result<NetworkCoordinator, Failure> {
        Ok(NetworkCoordinator::connect(
            &self.bundle,
            key,
            self.transport.clone(),
        )?)
    }
    /// What a tenant has stored on the coordinator.
    pub async fn usage(&self, tenant: PrivateKey) -> Result<crate::service::Usage, Failure> {
        Ok(self.client(tenant)?.usage().await?)
    }
    /// Closes a database's registration, refusing its later claims.
    pub async fn close(
        &self,
        scope: Scope,
        tenant: PrivateKey,
        id: Commitment,
    ) -> Result<(), Failure> {
        Ok(self.client(tenant)?.close_domain(scope, id).await?)
    }
    /// Publishes a control certificate for a database.
    pub async fn publish(
        &self,
        domain: Domain,
        request: ControlRequest,
        root: Option<Arc<dyn rocky_credentials::IdentityProvider>>,
    ) -> Result<ControlReceipt, Failure> {
        let _held = self.lock.lock().await;
        if request.trusted.is_empty() || request.trusted.len() > 32 {
            return Err("trusted must list 1 through 32 identity keys".into());
        }
        let (revision, grant, admission) = match request.revision {
            ControlRevision::Register { grant, admission } => {
                (1, grant.map(|g| *g), admission.map(|a| *a))
            }
            ControlRevision::Update(revision) if revision > 1 => (revision, None, None),
            ControlRevision::Update(_) => return Err("a later control revision is above 1".into()),
        };
        let genesis = self.bundle.genesis()?.genesis;
        let control = |time: u64| -> Result<DomainControl, Failure> {
            let (not_before, expires) = genesis.control_window(time, request.lifetime)?;
            Ok(DomainControl {
                scope: domain.scope,
                configuration: genesis.configuration,
                revision,
                trusted: request.trusted.clone(),
                publisher: Some(Box::new(domain.publisher)),
                not_before,
                expires,
            })
        };

        let inputs = rocky_core::commitment(
            "rocky.control-request.v1",
            &crate::certified::encode(&(control(0)?, &grant, &admission))?,
        );
        let time = self.signing_time(request.tenant.public_key(), request.id, inputs)?;
        let control = control(time)?;
        let expires = control.expires;
        let certificate = ControlCertificate::sign(control, &request.tenant)?;
        let client = self.client(request.tenant.clone())?;
        let reference = if revision == 1 {
            let grant = match (grant, root) {
                (Some(grant), _) => grant,
                (None, Some(root)) => SignedCoordinationGrant::sign(
                    crate::tenancy::CoordinationGrant {
                        publisher: rocky_credentials::Publisher(domain.publisher),
                        application: domain.application.clone(),
                        tenant: crate::tenancy::CoordinatorControl(request.tenant.public_key()),
                        not_before: time.saturating_sub(60),
                        expires: time.saturating_add(3600),
                    },
                    &*root,
                )?,
                (None, None) => {
                    return Err("registering needs a publisher coordination grant".into());
                }
            };
            grant.verify(time)?;
            if grant.grant.publisher.0 != domain.publisher
                || grant.grant.application != domain.application
                || grant.grant.tenant.0 != request.tenant.public_key()
            {
                return Err(
                    "the coordination grant names another publisher, application or tenant".into(),
                );
            }
            client
                .register(grant, admission, certificate, request.id)
                .await?
        } else {
            client.control(certificate, request.id).await?
        };
        Ok(ControlReceipt {
            tenant: request.tenant.public_key(),
            revision,
            control: reference,
            expires,
        })
    }
    fn signing_time(
        &self,
        tenant: PublicKey,
        id: Commitment,
        inputs: Commitment,
    ) -> Result<u64, Failure> {
        let path = self.directory.join("tenant.requests");
        let mut signed: Vec<Signed> = match rocky_platform::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error.into()),
        };
        let tenant = *tenant.as_bytes();
        if let Some(old) = signed
            .iter()
            .find(|s| s.tenant == tenant && s.id == id && s.inputs == inputs)
        {
            return Ok(old.time);
        }
        let time = crate::certified::now()?;
        if signed.len() >= 256 {
            signed.remove(0);
        }
        signed.push(Signed {
            tenant,
            id,
            inputs,
            time,
        });
        rocky_credentials::write_private(&path, &serde_json::to_vec(&signed)?)?;
        Ok(time)
    }
}
