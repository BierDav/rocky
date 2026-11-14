//! Tenant administration of a voter service, owned by the application.
use crate::{
    certified::{ControlCertificate, DomainControl, TrustBundle},
    service::NetworkCoordinator,
};
use rocky_core::{Commitment, Scope, net::Transport};
use rocky_credentials::{PrivateKey, PublicKey, SignedCoordinationGrant, SignedTenantAdmission};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::Mutex;

type Failure = Box<dyn std::error::Error + Send + Sync>;

pub struct ControlRequest {
    pub tenant: PrivateKey,
    pub trusted: Vec<PublicKey>,
    pub lifetime: u64,
    pub id: Commitment,
    pub revision: ControlRevision,
}
pub enum ControlRevision {
    Register {
        grant: Option<Box<SignedCoordinationGrant>>,
        admission: Option<Box<SignedTenantAdmission>>,
    },
    Update(u64),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlReceipt {
    pub tenant: PublicKey,
    pub revision: u64,
    pub control: Commitment,
    pub expires: u64,
}
/// Metadata the application obtains from its database and root signer.
pub struct Domain {
    pub scope: Scope,
    pub application: String,
    pub publisher: PublicKey,
}
#[derive(serde::Serialize, serde::Deserialize)]
struct Signed {
    tenant: [u8; 32],
    id: Commitment,
    inputs: Commitment,
    time: u64,
}

pub struct ControlClient {
    bundle: TrustBundle,
    transport: Arc<dyn Transport>,
    directory: PathBuf,
    lock: Mutex<()>,
}
impl ControlClient {
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
    pub async fn usage(&self, tenant: PrivateKey) -> Result<crate::service::Usage, Failure> {
        Ok(self.client(tenant)?.usage().await?)
    }
    pub async fn close(
        &self,
        scope: Scope,
        tenant: PrivateKey,
        id: Commitment,
    ) -> Result<(), Failure> {
        Ok(self.client(tenant)?.close_domain(scope, id).await?)
    }
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
        if !(60..=366 * 86_400).contains(&request.lifetime) {
            return Err("lifetime must be 60 s to 366 days".into());
        }
        let (revision, grant, admission) = match request.revision {
            ControlRevision::Register { grant, admission } => {
                (1, grant.map(|g| *g), admission.map(|a| *a))
            }
            ControlRevision::Update(revision) if revision > 1 => (revision, None, None),
            ControlRevision::Update(_) => return Err("a later control revision is above 1".into()),
        };
        let configuration = self.bundle.genesis()?.genesis.configuration;
        let control = |time: u64| DomainControl {
            scope: domain.scope,
            configuration,
            revision,
            trusted: request.trusted.clone(),
            publisher: Some(Box::new(domain.publisher)),
            not_before: time.saturating_sub(60),
            expires: time.saturating_add(request.lifetime),
        };

        let inputs = rocky_core::commitment(
            "rocky.control-request.v1",
            &crate::certified::encode(&(control(0), &grant, &admission))?,
        );
        let time = self.signing_time(request.tenant.public_key(), request.id, inputs)?;
        let certificate = ControlCertificate::sign(control(time), &request.tenant)?;
        let client = self.client(request.tenant.clone())?;
        let reference = if revision == 1 {
            let grant = match (grant, root) {
                (Some(grant), _) => grant,
                (None, Some(root)) => SignedCoordinationGrant::sign(
                    rocky_credentials::CoordinationGrant {
                        publisher: rocky_credentials::Publisher(domain.publisher),
                        application: domain.application.clone(),
                        tenant: rocky_credentials::CoordinatorControl(request.tenant.public_key()),
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
            expires: time.saturating_add(request.lifetime),
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
