use crate::{
    CoordinationError,
    certified::{ControlCertificate, DomainControl, TrustBundle, encode, now},
    client::{CALL_DEADLINE, NetworkCoordinator},
    counters::{Claim, Claimed, Name},
    request::{Operation, Outcome, REQUEST_LABEL, Registration, Request},
    tenancy::{CoordinationGrant, SignedCoordinationGrant, Tenant},
    transport::nonce,
};
use rocky_core::Commitment;
use rocky_credentials::{IdentityProvider, PrivateKey, PublicKey, Publisher};
use rocky_sdk::{
    Database,
    extension::{self, DeviceCredential},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

const GRANT_LIFETIME_SECONDS: u64 = 3600;

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
const RECEIPT_FORMAT: u8 = 1;
const RECEIPT_BYTES: usize = 1 + 32 + 8 + 32 + 8;
impl ControlReceipt {
    /// Its 81-byte encoding, to store with `Tenant::secret` until the next `renew`.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(RECEIPT_BYTES);
        bytes.push(RECEIPT_FORMAT);
        bytes.extend_from_slice(self.tenant.as_bytes());
        bytes.extend_from_slice(&self.revision.to_be_bytes());
        bytes.extend_from_slice(&self.control);
        bytes.extend_from_slice(&self.expires.to_be_bytes());
        bytes
    }
    /// Reads `to_bytes` back; another format is `UnsupportedFormat` and other damage `Corrupt`.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, CoordinationError> {
        let (&format, rest) = bytes.split_first().ok_or(CoordinationError::Corrupt)?;
        if format != RECEIPT_FORMAT {
            return Err(CoordinationError::UnsupportedFormat);
        }
        let (tenant, rest) = rest
            .split_first_chunk::<32>()
            .ok_or(CoordinationError::Corrupt)?;
        let (revision, rest) = rest
            .split_first_chunk::<8>()
            .ok_or(CoordinationError::Corrupt)?;
        let (control, rest) = rest
            .split_first_chunk::<32>()
            .ok_or(CoordinationError::Corrupt)?;
        let expires: &[u8; 8] = rest.try_into().map_err(|_| CoordinationError::Corrupt)?;
        Ok(Self {
            tenant: PublicKey::from_bytes(tenant).map_err(|_| CoordinationError::Corrupt)?,
            revision: u64::from_be_bytes(*revision),
            control: *control,
            expires: u64::from_be_bytes(*expires),
        })
    }
}

/// A failed `Counters` call: the coordinator's answer, the database's or the client endpoint's.
#[derive(Debug)]
pub enum CounterError {
    /// The coordinator was unreachable, refused or did not answer; see `CoordinationError`.
    Coordination(CoordinationError),
    /// The database could not supply or sign the device's credentials.
    Database(rocky_sdk::Error),
    /// `Counters::connect` could not bind its iroh client endpoint.
    Endpoint(rocky_core::net::Failure),
    /// `Counters::close` switched these counters off; connect again to come back online.
    Closed,
    /// `register` was given a key other than the database's root key, which only the owner holds.
    NotRootKey,
}
impl CounterError {
    /// Whether sending the same call again may succeed; after `OutcomeUnknown` only the same
    /// `Claim` is safe to send.
    pub fn retryable(&self) -> bool {
        match self {
            Self::Coordination(error) => error.retryable(),
            Self::Database(_) | Self::Endpoint(_) | Self::Closed | Self::NotRootKey => false,
        }
    }
}
impl std::fmt::Display for CounterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Coordination(error) => write!(f, "coordinator: {error}"),
            Self::Database(error) => write!(f, "database: {error}"),
            Self::Endpoint(error) => write!(f, "client endpoint: {error}"),
            Self::Closed => f.write_str("counters closed"),
            Self::NotRootKey => f.write_str("registering needs the database's root key"),
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

/// Gap-free named counters for one database, claimed through a coordinator; each counter is
/// one unit of uniqueness, so every door or till that must agree claims against the same name.
#[derive(Clone)]
pub struct Counters {
    shared: Arc<Shared>,
    deadline: Duration,
}
struct Shared {
    database: Database,
    client: NetworkCoordinator,
    endpoint: ClientEndpoint,
    closed: AtomicBool,
}
struct ClientEndpoint {
    attachment: rocky_iroh::Attachment,
    #[cfg(not(target_family = "wasm"))]
    home: Option<rocky_platform::task::Home>,
}
impl ClientEndpoint {
    async fn bind(bundle: &TrustBundle) -> Result<Self, CounterError> {
        let genesis = bundle.genesis()?;
        let public = genesis
            .genesis
            .public
            .as_ref()
            .ok_or(CoordinationError::Configuration)?;
        let options =
            rocky_iroh::Options::reaching(public.endpoints.iter().map(|e| e.route.as_str()))
                .map_err(|_| CoordinationError::Configuration)?;
        Ok(Self {
            attachment: options.client().await.map_err(CounterError::Endpoint)?,
            #[cfg(not(target_family = "wasm"))]
            home: rocky_platform::task::Home::current(),
        })
    }
}
impl Drop for ClientEndpoint {
    fn drop(&mut self) {
        let attachment = self.attachment.clone();
        let close = async move { attachment.close().await };
        #[cfg(target_family = "wasm")]
        drop(rocky_platform::task::spawn(close));
        #[cfg(not(target_family = "wasm"))]
        if let Some(home) = &self.home {
            drop(home.spawn(close));
        }
    }
}
impl Counters {
    /// Binds its own iroh client endpoint, with the relay or direct addresses the trust bundle's
    /// voters name, and reaches the coordinator through it until `close` or the last clone drops.
    pub async fn connect(database: Database, bundle: &TrustBundle) -> Result<Self, CounterError> {
        let endpoint = ClientEndpoint::bind(bundle).await?;
        let client = NetworkCoordinator::connect(
            bundle,
            PrivateKey::new(),
            endpoint.attachment.transport(),
        )?;
        Ok(Self {
            shared: Arc::new(Shared {
                database,
                client,
                endpoint,
                closed: AtomicBool::new(false),
            }),
            deadline: CALL_DEADLINE,
        })
    }
    /// Stops the client endpoint for every clone, so each later call fails at once with `Closed`,
    /// which is not retryable.
    pub async fn close(&self) {
        self.shared.closed.store(true, Ordering::SeqCst);
        self.shared.endpoint.attachment.close().await;
    }
    async fn device(&self) -> Result<DeviceCredential, CounterError> {
        if self.shared.closed.load(Ordering::SeqCst) {
            return Err(CounterError::Closed);
        }
        Ok(extension::device_credential(&self.shared.database).await?)
    }
    /// Bounds how long each call waits, 12 seconds (`CALL_DEADLINE`) by default.
    pub fn deadline(self, deadline: Duration) -> Self {
        Self { deadline, ..self }
    }
    /// Registers the database with the coordinator under `tenant`, signed by the owner's root key;
    /// `lifetime` is in seconds, at most the coordinator's `max_control_lifetime`.
    pub async fn register(
        &self,
        owner: &dyn IdentityProvider,
        tenant: &Tenant,
        lifetime: u64,
    ) -> Result<ControlReceipt, CounterError> {
        let device = self.device().await?;
        if owner.public_key().ok() != Some(device.publisher) {
            return Err(CounterError::NotRootKey);
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
        let device = self.device().await?;
        let revision = current
            .revision
            .checked_add(1)
            .ok_or(CoordinationError::Configuration)?;
        let certificate = self.control(&device, tenant, revision, now()?, lifetime)?;
        let operation = Operation::Control(Box::new(certificate.clone()));
        self.publish(tenant, certificate, operation).await
    }
    /// Takes the counter's next number for the claim's key, for any person with insert, update or
    /// delete rights; only `Claimed::First` admits, and after `OutcomeUnknown` the same `Claim`
    /// sent again answers the same `Claimed`.
    pub async fn claim(&self, counter: &Name, claim: &Claim) -> Result<Claimed, CounterError> {
        let device = self.device().await?;
        let operation = Operation::claim(device.scope, counter.clone(), *claim);
        match self.submit(device, operation).await? {
            Outcome::Claimed(claimed) => Ok(claimed),
            _ => Err(CoordinationError::Corrupt.into()),
        }
    }
    /// The counter's last number, or 0 before any.
    pub async fn read(&self, counter: &Name) -> Result<i64, CounterError> {
        let device = self.device().await?;
        let operation = Operation::Counter {
            scope: device.scope,
            counter: counter.clone(),
        };
        match self.submit(device, operation).await? {
            Outcome::Counter(value) => Ok(value),
            _ => Err(CoordinationError::Corrupt.into()),
        }
    }
    /// Tells the coordinator which devices this database revoked, so their claims are refused.
    pub async fn share_revocations(&self) -> Result<Commitment, CounterError> {
        let device = self.device().await?;
        Ok(self
            .shared
            .client
            .revoke(device.scope, device.revocations)
            .await?)
    }
    fn control(
        &self,
        device: &DeviceCredential,
        tenant: &Tenant,
        revision: u64,
        time: u64,
        lifetime: u64,
    ) -> Result<ControlCertificate, CoordinationError> {
        let genesis = &self.shared.client.certified_genesis().genesis;
        let (not_before, expires) = genesis.control_window(time, lifetime)?;
        ControlCertificate::sign(
            DomainControl {
                scope: device.scope,
                configuration: genesis.configuration,
                revision,
                admins: vec![device.publisher],
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
            self.shared.client.genesis()?,
            id,
            operation,
            (tenant.key(), None),
            now()?,
        )?;
        match self
            .shared
            .client
            .submit_within(&request, self.deadline)
            .await?
        {
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
            self.shared.client.genesis()?,
            nonce(),
            operation,
            (device.node, None, Some(device.chain)),
            now()?,
        );
        let signature = extension::sign(
            &self.shared.database,
            REQUEST_LABEL,
            &request.extension_body()?,
        )
        .await?;
        Ok(self
            .shared
            .client
            .submit_within(&request.with_signature(signature), self.deadline)
            .await?)
    }
}

#[cfg(all(test, not(target_family = "wasm")))]
mod tests {
    use super::*;

    #[test]
    fn a_control_receipt_reads_back_from_its_bytes_and_refuses_another_format() {
        let receipt = ControlReceipt {
            tenant: PrivateKey::new().public_key(),
            revision: 7,
            control: [9; 32],
            expires: 1_900_000_000,
        };
        let bytes = receipt.to_bytes();
        assert_eq!(bytes.len(), RECEIPT_BYTES);
        assert_eq!(ControlReceipt::from_bytes(&bytes), Ok(receipt));
        let mut other_format = bytes.clone();
        other_format[0] = RECEIPT_FORMAT + 1;
        assert_eq!(
            ControlReceipt::from_bytes(&other_format),
            Err(CoordinationError::UnsupportedFormat)
        );
        assert_eq!(
            ControlReceipt::from_bytes(&bytes[..RECEIPT_BYTES - 1]),
            Err(CoordinationError::Corrupt)
        );
        assert_eq!(
            ControlReceipt::from_bytes(&[bytes.as_slice(), &[0]].concat()),
            Err(CoordinationError::Corrupt)
        );
    }

    #[tokio::test]
    async fn dropping_the_last_handle_on_a_client_endpoint_stops_it() {
        let attachment = rocky_iroh::Options::isolated("127.0.0.1:0")
            .client()
            .await
            .unwrap();
        let endpoint = attachment.endpoint();
        let shared_by_two_clones = Arc::new(ClientEndpoint {
            attachment,
            home: rocky_platform::task::Home::current(),
        });
        drop(shared_by_two_clones.clone());
        assert!(
            !endpoint.is_closed(),
            "a clone dropping leaves the endpoint to the others"
        );
        drop(shared_by_two_clones);
        rocky_platform::time::timeout(Duration::from_secs(30), endpoint.closed())
            .await
            .expect("the last handle dropping closes the endpoint");
    }
}
