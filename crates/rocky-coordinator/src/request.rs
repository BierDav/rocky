use crate::tenancy::{SignedCoordinationGrant, SignedTenantAdmission};
use crate::{CoordinationError, Refusal, certified::*};
use p2panda_core::{PublicKey, Signature};
use rocky_core::*;
use rocky_credentials::{CredentialChain, RevocationSet};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[cfg(feature = "voter")]
pub const MAX_ADMINS: usize = 32;
pub const REQUEST_LABEL: &str = "rocky.ext.coordinator-request.v12";

/// What a coordinator request asks for.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Operation {
    /// Publishes a control certificate.
    Control(Box<ControlCertificate>),
    /// Changes the voters.
    Membership {
        /// The new voters.
        voters: BTreeSet<u64>,
        /// The voters the caller believes current.
        expected: BTreeSet<u64>,
        /// The membership revision the caller believes current.
        expected_revision: u64,
    },
    /// Closes a database's registration.
    Close {
        /// The database.
        scope: Scope,
    },
    /// Registers a database through a public service.
    Register(Box<Registration>),
    /// Reads the tenant's usage.
    Usage,
    /// Takes the next number of a counter.
    Claim {
        /// The database.
        scope: Scope,
        /// The counter.
        counter: crate::counters::Name,
        /// The commitment to the application's key.
        #[serde(with = "rocky_core::byte_arrays::one")]
        key: Commitment,
        /// The commitment to the application's attempt id, which decides between a first and a
        /// taken number.
        #[serde(with = "rocky_core::byte_arrays::one")]
        attempt: Commitment,
        /// Unix seconds when the key stops being in force.
        expires: u64,
    },
    /// Reads a counter's last number.
    Counter {
        /// The database.
        scope: Scope,
        /// The counter.
        counter: crate::counters::Name,
    },
    /// Pushes a database's revocations.
    Revoke {
        /// The database.
        scope: Scope,
        /// The revocations to merge.
        revocations: Box<RevocationSet>,
    },
}
impl Operation {
    /// Takes `counter`'s next number for `claim`.
    pub fn claim(
        scope: Scope,
        counter: crate::counters::Name,
        claim: crate::counters::Claim,
    ) -> Self {
        Self::Claim {
            scope,
            counter,
            key: claim.key,
            attempt: claim.attempt,
            expires: claim.expires,
        }
    }
}
/// A public registration: the root key's grant, the tenant's admission and the first certificate.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Registration {
    /// The root key's grant.
    pub grant: SignedCoordinationGrant,
    /// The provider's admission, if the service needs one.
    pub admission: Option<SignedTenantAdmission>,
    /// The first control certificate.
    pub certificate: ControlCertificate,
}
/// A signed coordinator request.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Request {
    /// The genesis id it is bound to.
    #[serde(with = "rocky_core::byte_arrays::one")]
    pub genesis: Commitment,
    /// The signing key.
    pub key: PublicKey,
    /// A capability that certifies `key`, if any.
    pub certificate: Option<rocky_credentials::SignedCapability>,
    /// The device's membership chain, for device calls.
    pub credentials: Option<CredentialChain>,
    /// Makes a retry apply once.
    #[serde(with = "rocky_core::byte_arrays::one")]
    pub id: Commitment,
    /// Unix seconds when it was made; more than 30 seconds off the coordinator's clock is refused.
    pub time: u64,
    /// What it asks for.
    pub operation: Operation,
    signature: Signature,
}
#[cfg(test)]
pub(crate) fn self_certificate(
    key: &p2panda_core::PrivateKey,
) -> rocky_credentials::SignedCapability {
    self_certificate_in(
        key,
        rocky_credentials::DatabaseContext {
            publisher: key.public_key(),
            scope: Scope {
                mesh: MeshId([1; 16]),
                app: AppId([2; 16]),
                domain: DomainId([3; 16]),
            },
        },
    )
}
#[cfg(test)]
pub(crate) fn self_certificate_in(
    key: &p2panda_core::PrivateKey,
    database: rocky_credentials::DatabaseContext,
) -> rocky_credentials::SignedCapability {
    use rocky_credentials::{Capability, Extended, Extension, PeerId};
    let capability = Capability {
        subject: key.public_key(),
        kind: rocky_core::PrincipalKind::Node,
        grants: Vec::new(),
        not_before: 0,
        expires: rocky_credentials::NEVER,
        delegations: 0,
        extension: Extension::new(Extended {
            node: Some(PeerId::of(key.public_key())),
            database: Some(database),
            sequence: Some(0),
            possession: Some(
                rocky_credentials::NodePossession::sign(key, key.public_key(), database)
                    .expect("possession"),
            ),
            ..Extended::default()
        })
        .expect("extension"),
    };
    rocky_credentials::SignedCapability::sign(capability, key).expect("certificate")
}
#[cfg(test)]
pub(crate) fn self_certified(
    genesis: Commitment,
    id: Commitment,
    operation: Operation,
    key: &p2panda_core::PrivateKey,
    time: u64,
) -> Result<Request, CoordinationError> {
    Request::certified(
        genesis,
        id,
        operation,
        (key, Some(self_certificate(key))),
        time,
    )
}
impl Request {
    /// Signs a request with an optional certifying capability.
    pub fn certified(
        genesis: Commitment,
        id: Commitment,
        operation: Operation,
        (key, certificate): (
            &p2panda_core::PrivateKey,
            Option<rocky_credentials::SignedCapability>,
        ),
        time: u64,
    ) -> Result<Self, CoordinationError> {
        Self::unsigned(
            genesis,
            id,
            operation,
            (key.public_key(), certificate, None),
            time,
        )
        .signed(key)
    }
    /// An unsigned request, for a signer outside this process such as a database.
    pub fn unsigned(
        genesis: Commitment,
        id: Commitment,
        operation: Operation,
        (key, certificate, credentials): (
            PublicKey,
            Option<rocky_credentials::SignedCapability>,
            Option<CredentialChain>,
        ),
        time: u64,
    ) -> Self {
        Self {
            genesis,
            key,
            certificate,
            credentials,
            id,
            time,
            operation,
            signature: Signature::from_bytes(&[0; 64]),
        }
    }
    /// Signs with `key`, which must be the request's key.
    pub fn signed(mut self, key: &p2panda_core::PrivateKey) -> Result<Self, CoordinationError> {
        if key.public_key() != self.key {
            return Err(CoordinationError::Denied(Refusal::Signature));
        }
        self.signature = key.sign(&self.signing_bytes()?);
        Ok(self)
    }
    /// Attaches a signature made elsewhere over `extension_body`.
    pub fn with_signature(mut self, signature: Signature) -> Self {
        self.signature = signature;
        self
    }
    /// The bytes an external signer signs.
    pub fn extension_body(&self) -> Result<Vec<u8>, CoordinationError> {
        encode(&(
            self.genesis,
            self.key,
            &self.certificate,
            &self.credentials,
            self.id,
            self.time,
            &self.operation,
        ))
    }
    fn signing_bytes(&self) -> Result<Vec<u8>, CoordinationError> {
        rocky_credentials::extension_message(REQUEST_LABEL, &self.extension_body()?)
            .map_err(|_| CoordinationError::Corrupt)
    }
    /// The largest encoded client request.
    pub const MAX_CLIENT_BYTES: usize = 16 * 1024;
    /// Refuses a request over `MAX_CLIENT_BYTES`.
    pub fn within_client_limit(&self) -> Result<(), CoordinationError> {
        if encode(self)?.len() > Self::MAX_CLIENT_BYTES {
            return Err(CoordinationError::Denied(Refusal::Oversized));
        }
        Ok(())
    }
    /// Whether the request only reads.
    pub fn read_only(&self) -> bool {
        matches!(self.operation, Operation::Usage | Operation::Counter { .. })
    }
}
#[cfg(feature = "voter")]
impl Request {
    /// Signs a request now.
    pub fn sign(
        genesis: Commitment,
        id: Commitment,
        operation: Operation,
        key: &p2panda_core::PrivateKey,
    ) -> Result<Self, CoordinationError> {
        Self::sign_at(genesis, id, operation, key, now()?)
    }
    /// Signs a request at `time`.
    pub fn sign_at(
        genesis: Commitment,
        id: Commitment,
        operation: Operation,
        key: &p2panda_core::PrivateKey,
        time: u64,
    ) -> Result<Self, CoordinationError> {
        Self::certified(genesis, id, operation, (key, None), time)
    }
    /// The identity the certificate vouches for at `time`.
    pub fn identity(&self, time: u64) -> Option<PublicKey> {
        self.certificate.as_ref()?.certifies(self.key, time).ok()
    }
    /// Checks the genesis and signature.
    pub fn authenticate(&self, genesis: Commitment) -> Result<(), CoordinationError> {
        if self.genesis != genesis || !self.key.verify(&self.signing_bytes()?, &self.signature) {
            return Err(CoordinationError::Denied(Refusal::Signature));
        }
        Ok(())
    }
    /// Refuses a request more than 30 seconds off the local clock.
    pub fn fresh(&self) -> Result<(), CoordinationError> {
        let coordinator_time = now()?;
        if coordinator_time.abs_diff(self.time) > 30 {
            return Err(CoordinationError::ClockSkew { coordinator_time });
        }
        Ok(())
    }
    pub(crate) fn digest(&self) -> Result<Commitment, CoordinationError> {
        Ok(commitment(
            "rocky.coordination-operation.v1",
            &encode(&self.operation)?,
        ))
    }
    pub(crate) fn keeps_retry_outcome(&self) -> bool {
        !matches!(
            self.operation,
            Operation::Usage
                | Operation::Claim { .. }
                | Operation::Counter { .. }
                | Operation::Revoke { .. }
        )
    }
}
/// A coordinator's answer to a request.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Outcome {
    /// What a claim found.
    Claimed(crate::counters::Claimed),
    /// A counter's last number, or 0 before any.
    Counter(i64),
    /// The digest of the merged revocations.
    Revoked(#[serde(with = "rocky_core::byte_arrays::one")] Commitment),
    /// The reference of the accepted control certificate.
    Controlled(#[serde(with = "rocky_core::byte_arrays::one")] Commitment),
    /// The voters after a membership change.
    Membership(BTreeSet<u64>),
    /// The registration was closed.
    Closed,
    /// The tenant's usage.
    Usage(Usage),
}
/// A voter's answer to a client request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Reply {
    /// The leader the voter knows of, so the client asks it next.
    pub leader: Option<u64>,
    /// The outcome.
    pub result: Result<Outcome, CoordinationError>,
}
/// What a tenant holds on the coordinator.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Usage {
    /// Registered databases that are open.
    pub open_domains: usize,
    /// Registered databases that were closed.
    pub closed_domains: usize,
    /// Accounted bytes of state.
    pub bytes: usize,
    /// Registrations made today.
    pub registrations_today: u32,
}
