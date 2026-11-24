use crate::{
    CoordinationError, Refusal,
    certified::{ControlCertificate, DomainControl, TrustBundle, encode, now},
    control::ControlReceipt,
    counters::{Claim, Name, Reading},
    machine::{REQUEST_LABEL, Request},
    service::{CALL_DEADLINE, NetworkCoordinator, Operation, Outcome, Registration},
    tenancy::{CoordinationGrant, SignedCoordinationGrant, Tenant},
    transport::nonce,
};
use rocky_core::Commitment;
use rocky_credentials::{IdentityProvider, PrivateKey, Publisher};
use rocky_platform::time::Duration;
use rocky_sdk::{Database, DeviceCredential, transport::Transport};
use std::sync::Arc;

const GRANT_LIFETIME_SECONDS: u64 = 3600;

/// A failed `Counters` call: the coordinator's answer or the database's.
#[derive(Debug)]
pub enum CounterError {
    /// The coordinator was unreachable or refused; `Unavailable` took no number.
    Coordination(CoordinationError),
    /// The database could not supply or sign the device's credentials.
    Database(rocky_sdk::Error),
}
impl std::fmt::Display for CounterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Coordination(error) => write!(f, "coordinator: {error}"),
            Self::Database(error) => write!(f, "database: {error}"),
        }
    }
}
impl std::error::Error for CounterError {}
impl From<CoordinationError> for CounterError {
    fn from(error: CoordinationError) -> Self {
        Self::Coordination(error)
    }
}
impl From<rocky_sdk::Error> for CounterError {
    fn from(error: rocky_sdk::Error) -> Self {
        Self::Database(error)
    }
}

/// Gap-free named counters for one database, claimed through a coordinator.
pub struct Counters {
    database: Database,
    client: Arc<NetworkCoordinator>,
    deadline: Duration,
}
impl Counters {
    /// Claims through a coordinator client the application built.
    pub fn new(database: Database, client: Arc<NetworkCoordinator>) -> Self {
        Self {
            database,
            client,
            deadline: CALL_DEADLINE,
        }
    }
    /// Reaches the coordinator a trust bundle names over `transport`, whose kind the bundle must
    /// list.
    pub fn connect(
        database: Database,
        bundle: &TrustBundle,
        transport: Arc<dyn Transport>,
    ) -> Result<Self, CounterError> {
        let session = PrivateKey::new();
        let client = NetworkCoordinator::connect(bundle, session, transport)?;
        Ok(Self::new(database, Arc::new(client)))
    }
    /// Bounds how long each call waits, 12 seconds by default.
    pub fn with_deadline(self, deadline: Duration) -> Self {
        Self { deadline, ..self }
    }
    /// Registers the database with the coordinator under `tenant`, signed by the database's root
    /// key; `lifetime` is in seconds, at most the coordinator's `max_control_lifetime`.
    pub async fn register(
        &self,
        owner: &dyn IdentityProvider,
        tenant: &Tenant,
        lifetime: u64,
    ) -> Result<ControlReceipt, CounterError> {
        let device = self.database.device_credential().await?;
        if owner.public_key().ok() != Some(device.publisher) {
            return Err(CoordinationError::Denied(Refusal::InvalidGrant).into());
        }
        let time = now()?;
        let certificate = self.control(&device, tenant, 1, time, lifetime)?;
        let grant = SignedCoordinationGrant::sign(
            CoordinationGrant {
                publisher: Publisher(device.publisher),
                application: device.application,
                tenant: tenant.control(),
                not_before: certificate.control.not_before,
                expires: time.saturating_add(GRANT_LIFETIME_SECONDS),
            },
            owner,
        )?;
        let operation = Operation::Register(Box::new(Registration {
            grant,
            admission: tenant.admission().cloned(),
            certificate: certificate.clone(),
        }));
        self.publish(tenant, certificate, operation).await
    }
    /// Extends a registration before it expires, by another `lifetime` seconds.
    pub async fn renew(
        &self,
        tenant: &Tenant,
        current: &ControlReceipt,
        lifetime: u64,
    ) -> Result<ControlReceipt, CounterError> {
        let device = self.database.device_credential().await?;
        let revision = current
            .revision
            .checked_add(1)
            .ok_or(CoordinationError::Configuration)?;
        let certificate = self.control(&device, tenant, revision, now()?, lifetime)?;
        let operation = Operation::Control(Box::new(certificate.clone()));
        self.publish(tenant, certificate, operation).await
    }
    /// Takes the next number of a counter for any person with write access; a retried
    /// `Claim::keyed` returns its first number.
    pub async fn claim(&self, counter: &Name, claim: Claim) -> Result<u64, CounterError> {
        let device = self.database.device_credential().await?;
        let operation = Operation::Claim {
            scope: device.scope,
            counter: counter.clone(),
            claim,
        };
        match self.submit(device, operation).await? {
            Outcome::Claimed(value) => Ok(value),
            _ => Err(CoordinationError::Corrupt.into()),
        }
    }
    /// Reads a counter's last number and, for `claim`, whether it took one.
    pub async fn read(
        &self,
        counter: &Name,
        claim: Option<Claim>,
    ) -> Result<Reading, CounterError> {
        let device = self.database.device_credential().await?;
        let operation = Operation::Counter {
            scope: device.scope,
            counter: counter.clone(),
            claim,
        };
        match self.submit(device, operation).await? {
            Outcome::Counter(reading) => Ok(reading),
            _ => Err(CoordinationError::Corrupt.into()),
        }
    }
    /// Tells the coordinator which devices this database revoked, so their claims are refused.
    pub async fn share_revocations(&self) -> Result<Commitment, CounterError> {
        let device = self.database.device_credential().await?;
        Ok(self.client.revoke(device.scope, device.revocations).await?)
    }
    fn control(
        &self,
        device: &DeviceCredential,
        tenant: &Tenant,
        revision: u64,
        time: u64,
        lifetime: u64,
    ) -> Result<ControlCertificate, CoordinationError> {
        let genesis = &self.client.certified_genesis().genesis;
        let (not_before, expires) = genesis.control_window(time, lifetime)?;
        ControlCertificate::sign(
            DomainControl {
                scope: device.scope,
                configuration: genesis.configuration,
                revision,
                trusted: vec![device.publisher],
                publisher: Some(Box::new(device.publisher)),
                not_before,
                expires,
            },
            tenant.key(),
        )
    }
    async fn publish(
        &self,
        tenant: &Tenant,
        certificate: ControlCertificate,
        operation: Operation,
    ) -> Result<ControlReceipt, CounterError> {
        let id = rocky_core::commitment("rocky.counters-control.v1", &encode(&certificate)?);
        let request = Request::certified(
            self.client.genesis()?,
            id,
            operation,
            (tenant.key(), None),
            now()?,
        )?;
        match self.client.submit_within(&request, self.deadline).await? {
            Outcome::Controlled(control) => Ok(ControlReceipt {
                tenant: certificate.issuer,
                revision: certificate.control.revision,
                control,
                expires: certificate.control.expires,
            }),
            _ => Err(CoordinationError::Corrupt.into()),
        }
    }
    async fn submit(
        &self,
        device: DeviceCredential,
        operation: Operation,
    ) -> Result<Outcome, CounterError> {
        let request = Request::unsigned(
            self.client.genesis()?,
            nonce(),
            operation,
            (device.node, None, Some(device.chain)),
            now()?,
        );
        let signature = self
            .database
            .sign_extension(REQUEST_LABEL, &request.extension_body()?)
            .await?;
        Ok(self
            .client
            .submit_within(&request.with_signature(signature), self.deadline)
            .await?)
    }
}
