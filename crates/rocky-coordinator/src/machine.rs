use crate::tenancy::{SignedCoordinationGrant, SignedTenantAdmission};
use crate::{certified::*, retention::DAY, *};
use p2panda_core::{PublicKey, Signature};
use rocky_core::Right;
use rocky_credentials::{CredentialChain, RevocationSet};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const ACCOUNTED_DOMAIN_BYTES: usize = 1024;
const ACCOUNTED_TENANT_BYTES: usize = 64;
pub const MAX_PUBLIC_DOMAINS: usize = 65536;
pub const MAX_ADMINS: usize = 32;
pub const REQUEST_LABEL: &str = "rocky.ext.coordinator-request.v10";

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Domain {
    scope: Scope,
    counters: counters::Counters,
    revocations: RevocationSet,
    revocation_bytes: usize,
    closed: bool,
}
impl Domain {
    fn new(scope: Scope) -> Self {
        Self {
            scope,
            counters: counters::Counters::default(),
            revocations: RevocationSet::new(),
            revocation_bytes: 0,
            closed: false,
        }
    }
    fn open(&self) -> bool {
        !self.closed
    }
    fn bytes(&self) -> usize {
        self.counters.bytes() + self.revocation_bytes
    }
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct State {
    domains: Vec<Domain>,
    limits: Limits,
    time: u64,
}
impl State {
    fn new(limits: Limits) -> Self {
        Self {
            domains: Vec::new(),
            limits,
            time: 0,
        }
    }
    fn advance(&mut self, time: u64) {
        self.time = self.time.max(time);
    }
    fn retention(&self) -> Retention {
        Retention {
            domains: self.domains.len(),
            bytes: self.bytes(),
        }
    }
    fn bytes(&self) -> usize {
        self.domains.iter().map(Domain::bytes).sum()
    }
    fn domain(&mut self, scope: Scope) -> Result<&mut Domain, CoordinationError> {
        self.domains
            .iter_mut()
            .find(|d| d.scope == scope)
            .ok_or(CoordinationError::UnknownDomain)
    }
    fn find(&self, scope: Scope) -> Result<&Domain, CoordinationError> {
        self.domains
            .iter()
            .find(|d| d.scope == scope)
            .ok_or(CoordinationError::UnknownDomain)
    }
    fn close(&mut self, scope: Scope) -> Result<(), CoordinationError> {
        self.domain(scope)?.closed = true;
        Ok(())
    }
}

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
        /// The claim, which makes a retry return its first number.
        claim: crate::counters::Claim,
    },
    /// Reads a counter.
    Counter {
        /// The database.
        scope: Scope,
        /// The counter.
        counter: crate::counters::Name,
        /// A claim to look up, if any.
        claim: Option<crate::counters::Claim>,
    },
    /// Pushes a database's revocations.
    Revoke {
        /// The database.
        scope: Scope,
        /// The revocations to merge.
        revocations: Box<RevocationSet>,
    },
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
    pub genesis: Commitment,
    /// The signing key.
    pub key: PublicKey,
    /// A capability that certifies `key`, if any.
    pub certificate: Option<rocky_credentials::SignedCapability>,
    /// The device's membership chain, for device calls.
    pub credentials: Option<CredentialChain>,
    /// Makes a retry apply once.
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
    /// The largest encoded client request.
    pub const MAX_CLIENT_BYTES: usize = 16 * 1024;
    /// Refuses a request over `MAX_CLIENT_BYTES`.
    pub fn within_client_limit(&self) -> Result<(), CoordinationError> {
        if encode(self)?.len() > Self::MAX_CLIENT_BYTES {
            return Err(CoordinationError::Denied(Refusal::Oversized));
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
    fn digest(&self) -> Result<Commitment, CoordinationError> {
        Ok(commitment(
            "rocky.coordination-operation.v1",
            &encode(&self.operation)?,
        ))
    }
    fn keeps_retry_outcome(&self) -> bool {
        !matches!(
            self.operation,
            Operation::Usage
                | Operation::Claim { .. }
                | Operation::Counter { .. }
                | Operation::Revoke { .. }
        )
    }
    /// Whether the request only reads.
    pub fn read_only(&self) -> bool {
        matches!(self.operation, Operation::Usage | Operation::Counter { .. })
    }
}
/// A coordinator's answer to a request.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Outcome {
    /// The number a claim took.
    Claimed(u64),
    /// A counter's state.
    Counter(crate::counters::Reading),
    /// The digest of the merged revocations.
    Revoked(Commitment),
    /// The reference of the accepted control certificate.
    Controlled(Commitment),
    /// The voters after a membership change.
    Membership(BTreeSet<u64>),
    /// The registration was closed.
    Closed,
    /// The tenant's usage.
    Usage(Usage),
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
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Retained {
    key: PublicKey,
    id: Commitment,
    time: u64,
    digest: Commitment,
    result: Result<Outcome, CoordinationError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tenant: Option<PublicKey>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Tenant {
    key: PublicKey,
    day: u64,
    registrations: u32,
}
/// The coordinator's replicated state machine.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Metadata {
    /// The deployment's genesis.
    pub genesis: CertifiedGenesis,
    controls: Vec<ControlCertificate>,
    state: State,
    retained: Vec<Retained>,
    /// The voters membership changes aim for.
    pub desired_voters: BTreeSet<u64>,
    membership_revision: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tenants: Vec<Tenant>,
}
impl Metadata {
    /// An empty state machine with the default limits.
    #[cfg(test)]
    pub fn new(genesis: CertifiedGenesis) -> Result<Self, CoordinationError> {
        Self::with_limits(genesis, Limits::default())
    }
    /// An empty state machine with these limits.
    pub fn with_limits(
        genesis: CertifiedGenesis,
        limits: Limits,
    ) -> Result<Self, CoordinationError> {
        limits.validate()?;
        Ok(Self {
            desired_voters: genesis.genesis.initial_voters.clone(),
            membership_revision: 0,
            genesis,
            controls: Vec::new(),
            state: State::new(limits),
            retained: Vec::new(),
            tenants: Vec::new(),
        })
    }
    /// The last applied time.
    pub fn time(&self) -> u64 {
        self.state.time
    }
    /// Registered tenants.
    pub fn tenants(&self) -> usize {
        self.tenants.len()
    }
    /// What counters and domains hold.
    pub fn retention(&self) -> Retention {
        self.state.retention()
    }
    /// Remembered request outcomes.
    pub fn retry_records(&self) -> usize {
        self.retained.len()
    }
    /// Authenticates and authorizes a request before it is proposed.
    pub fn preflight(&self, request: &Request) -> Result<(), CoordinationError> {
        request.authenticate(self.genesis.id()?)?;
        request.fresh()?;
        self.authorize_authenticated(request)
    }
    /// Authorizes a request whose signature was already checked.
    pub fn authorize_authenticated(&self, request: &Request) -> Result<(), CoordinationError> {
        if self
            .retained
            .iter()
            .any(|r| r.key == request.key && r.id == request.id)
        {
            return Ok(());
        }
        let time = request.time;
        match &request.operation {
            Operation::Control(c) => {
                if c.issuer != request.key {
                    return Err(CoordinationError::Denied(Refusal::NotDomainAuthority));
                }
                self.control_authorized(c, time)
            }
            Operation::Membership { .. } => {
                if request.key == self.genesis.genesis.membership_authority {
                    Ok(())
                } else {
                    Err(CoordinationError::Denied(Refusal::NotMembershipAuthority))
                }
            }
            Operation::Close { scope } => self.closure_authorized(request, *scope),
            Operation::Register(registration) => {
                let Registration {
                    grant,
                    admission,
                    certificate,
                } = &**registration;
                self.registration_authorized(request, grant, admission.as_ref(), certificate, time)
            }
            Operation::Usage => self.tenant(request.key).map(|_| ()),
            Operation::Claim { scope, .. } => self.counter_authorized(*scope, true, request, time),
            Operation::Counter { scope, .. } => {
                self.counter_authorized(*scope, false, request, time)
            }
            Operation::Revoke { scope, revocations } => self
                .merged_revocations(*scope, revocations, time)
                .map(|_| ()),
        }
    }
    /// Applies a request at `time`.
    pub fn apply_at(&mut self, time: u64, request: &Request) -> Result<Outcome, CoordinationError> {
        let authenticated = self
            .genesis
            .id()
            .and_then(|genesis| request.authenticate(genesis));
        self.apply_authenticated(time, request, authenticated)
    }
    /// Applies a request with the result of an earlier signature check.
    pub fn apply_authenticated(
        &mut self,
        time: u64,
        request: &Request,
        authenticated: Result<(), CoordinationError>,
    ) -> Result<Outcome, CoordinationError> {
        if time > self.state.time {
            self.state.advance(time);
            let window = self.state.limits.retry_window;
            self.retained
                .retain(|r| r.time.saturating_add(window) >= time);
        }
        authenticated?;
        let digest = request.digest()?;
        if let Some(old) = self
            .retained
            .iter()
            .find(|r| r.key == request.key && r.id == request.id)
        {
            return if old.digest == digest {
                old.result.clone()
            } else {
                Err(CoordinationError::RequestConflict)
            };
        }
        let retry_record_may_be_gone =
            request.time.saturating_add(self.state.limits.retry_window) < self.state.time;
        if retry_record_may_be_gone {
            return Err(CoordinationError::Expired);
        }
        let tenant = self.retry_tenant(request);
        if request.keeps_retry_outcome()
            && (self.retained.len() >= MAX_RECORDS
                || self
                    .retained
                    .iter()
                    .filter(|r| r.key == request.key && r.tenant == tenant)
                    .count()
                    >= self.state.limits.retry_records
                || tenant.is_some_and(|tenant| {
                    self.retained
                        .iter()
                        .filter(|r| r.tenant == Some(tenant))
                        .count()
                        >= self.state.limits.retry_records
                }))
        {
            return Err(CoordinationError::Backpressure);
        }
        let result = self.transition(request);
        let refused_registration =
            matches!(request.operation, Operation::Register(_)) && result.is_err();
        if request.keeps_retry_outcome() && !refused_registration {
            self.retained.push(Retained {
                key: request.key,
                id: request.id,
                time: request.time,
                digest,
                result: result.clone(),
                tenant,
            });
        }
        result
    }
    /// Answers a read-only request without changing state.
    pub fn read(&self, request: &Request) -> Result<Outcome, CoordinationError> {
        request.authenticate(self.genesis.id()?)?;
        self.read_authenticated(request)
    }
    /// Answers a read-only request whose signature was already checked.
    pub fn read_authenticated(&self, request: &Request) -> Result<Outcome, CoordinationError> {
        match &request.operation {
            Operation::Usage => self.usage(request.key).map(Outcome::Usage),
            Operation::Counter {
                scope,
                counter,
                claim,
            } => self.counter(*scope, counter, *claim, request),
            _ => Err(CoordinationError::Denied(Refusal::UnexpectedMessage)),
        }
    }
    fn counter_authorized(
        &self,
        scope: Scope,
        claim: bool,
        request: &Request,
        time: u64,
    ) -> Result<(), CoordinationError> {
        let control = self.current_control(scope, time)?;
        if !self.calls_as_admin(control, request, time) {
            self.member(control, claim, request, time)
                .map_err(CoordinationError::Denied)?;
        }
        if claim && !self.state.find(scope)?.open() {
            return Err(CoordinationError::Denied(Refusal::ClosedDomain));
        }
        Ok(())
    }
    fn member(
        &self,
        control: &DomainControl,
        writes: bool,
        request: &Request,
        time: u64,
    ) -> Result<(), Refusal> {
        let (Some(publisher), Some(chain), Ok(domain)) = (
            control.publisher.as_deref().copied(),
            request.credentials.as_ref(),
            self.state.find(control.scope),
        ) else {
            return Err(Refusal::NotMember);
        };
        if chain
            .verify_database(rocky_credentials::DatabaseContext {
                publisher,
                scope: control.scope,
            })
            .is_err()
        {
            return Err(Refusal::NotMember);
        }
        let person = match chain.node_holder(publisher, request.key, &domain.revocations, time) {
            Ok(person) => person,
            Err(_)
                if chain
                    .node_holder(publisher, request.key, &RevocationSet::new(), time)
                    .is_ok() =>
            {
                return Err(Refusal::Revoked);
            }
            Err(_) => return Err(Refusal::NotMember),
        };
        let writing =
            |right: &Right| matches!(right, Right::Insert | Right::Update | Right::Delete);
        let may_write = person.extended().is_ok_and(|extended| {
            extended
                .whole
                .is_some_and(|whole| whole.rights().iter().any(writing))
        }) || person
            .grants
            .iter()
            .any(|grant| grant.scope.domain == control.scope && grant.rights.iter().any(writing));
        if writes && !may_write {
            return Err(Refusal::ReadOnly);
        }
        Ok(())
    }
    fn merged_revocations(
        &self,
        scope: Scope,
        pushed: &RevocationSet,
        time: u64,
    ) -> Result<RevocationSet, CoordinationError> {
        let publisher = self
            .controls
            .iter()
            .find(|c| c.control.scope == scope)
            .ok_or(CoordinationError::UnknownDomain)?
            .control
            .publisher
            .as_deref()
            .copied()
            .ok_or(CoordinationError::Denied(Refusal::NoPublisher))?;
        let domain = self.state.find(scope)?;
        if !domain.open() {
            return Err(CoordinationError::Denied(Refusal::ClosedDomain));
        }
        pushed
            .verify_in(rocky_credentials::DatabaseContext { publisher, scope })
            .map_err(|_| CoordinationError::Denied(Refusal::InvalidRevocations))?;
        let mut merged = domain.revocations.clone();
        merged
            .merge(pushed, publisher, scope.app, time)
            .map_err(|_| CoordinationError::Denied(Refusal::InvalidRevocations))?;
        Ok(merged)
    }
    fn revoke(
        &mut self,
        scope: Scope,
        pushed: &RevocationSet,
    ) -> Result<Outcome, CoordinationError> {
        let time = self.state.time;
        let merged = self.merged_revocations(scope, pushed, time)?;
        let bytes = encode(&merged)?.len();
        let growth = bytes.saturating_sub(self.state.find(scope)?.revocation_bytes);
        if growth > self.tenant_available(scope)?
            || growth
                > self
                    .state
                    .limits
                    .total_bytes
                    .saturating_sub(self.total_used()?)
        {
            return Err(CoordinationError::Backpressure);
        }
        let digest = merged
            .digest(time)
            .map_err(|_| CoordinationError::Corrupt)?;
        let domain = self.state.domain(scope)?;
        domain.revocations = merged;
        domain.revocation_bytes = bytes;
        Ok(Outcome::Revoked(digest))
    }
    fn counter(
        &self,
        scope: Scope,
        counter: &crate::counters::Name,
        claim: Option<crate::counters::Claim>,
        request: &Request,
    ) -> Result<Outcome, CoordinationError> {
        let time = self.state.time;
        self.counter_authorized(scope, false, request, time)?;
        let domain = self.state.find(scope)?;
        domain
            .counters
            .read(counter, claim, time)
            .map(Outcome::Counter)
    }
    fn claim(
        &mut self,
        scope: Scope,
        counter: &crate::counters::Name,
        claim: crate::counters::Claim,
        request: &Request,
    ) -> Result<Outcome, CoordinationError> {
        let time = self.state.time;
        self.counter_authorized(scope, true, request, time)?;
        let available = self.tenant_available(scope)?.min(
            self.state
                .limits
                .total_bytes
                .saturating_sub(self.total_used()?),
        );
        self.state
            .domain(scope)?
            .counters
            .claim(counter, claim, time, available)
            .map(Outcome::Claimed)
    }
    fn issuer(&self, scope: Scope) -> Option<PublicKey> {
        self.controls
            .iter()
            .find(|c| c.control.scope == scope)
            .map(|c| c.issuer)
    }
    fn tenant(&self, key: PublicKey) -> Result<&Tenant, CoordinationError> {
        self.tenants
            .iter()
            .find(|t| t.key == key)
            .ok_or(CoordinationError::Denied(Refusal::UnknownTenant))
    }
    fn genesis_bound(&self, scope: Scope) -> bool {
        self.genesis
            .genesis
            .authorities
            .iter()
            .any(|a| a.scope == scope)
    }
    fn is_scope_authority(&self, scope: Scope, key: PublicKey) -> bool {
        if self.genesis_bound(scope) {
            return self
                .genesis
                .genesis
                .authorities
                .iter()
                .any(|a| a.scope == scope && a.issuer == key);
        }
        self.issuer(scope) == Some(key) && self.tenants.iter().any(|t| t.key == key)
    }
    pub(crate) fn retry_tenant(&self, request: &Request) -> Option<PublicKey> {
        self.genesis.genesis.public.as_ref()?;
        let (scope, counter) = match &request.operation {
            Operation::Register(_) | Operation::Usage => return Some(request.key),
            Operation::Membership { .. } => return None,
            Operation::Control(certificate) => (certificate.control.scope, false),
            Operation::Close { scope } | Operation::Revoke { scope, .. } => (*scope, false),
            Operation::Claim { scope, .. } | Operation::Counter { scope, .. } => (*scope, true),
        };
        let certificate = self.controls.iter().find(|c| c.control.scope == scope)?;
        if certificate.issuer != request.key
            && !self.calls_as_admin(&certificate.control, request, request.time)
            && !(counter
                && self
                    .member(&certificate.control, false, request, request.time)
                    .is_ok())
        {
            return None;
        }
        self.tenant(certificate.issuer).ok().map(|t| t.key)
    }
    /// The tenant whose lane a request uses, if any.
    pub fn tenant_lane(&self, request: &Request) -> Option<PublicKey> {
        self.tenant(self.retry_tenant(request)?).ok().map(|t| t.key)
    }
    fn certificate_bytes(certificate: &ControlCertificate) -> Result<usize, CoordinationError> {
        Ok(ACCOUNTED_DOMAIN_BYTES + encode(certificate)?.len())
    }
    fn tenant_used(&self, issuer: PublicKey) -> Result<usize, CoordinationError> {
        let mut used =
            usize::from(self.tenants.iter().any(|t| t.key == issuer)) * ACCOUNTED_TENANT_BYTES;
        for certificate in self.controls.iter().filter(|c| c.issuer == issuer) {
            used += Self::certificate_bytes(certificate)?;
            if let Some(domain) = self
                .state
                .domains
                .iter()
                .find(|d| d.scope == certificate.control.scope)
            {
                used += domain.bytes();
            }
        }
        Ok(used)
    }
    /// Accounted bytes of all state.
    pub fn total_used(&self) -> Result<usize, CoordinationError> {
        let mut used = self.state.bytes() + self.tenants.len() * ACCOUNTED_TENANT_BYTES;
        for certificate in &self.controls {
            used += Self::certificate_bytes(certificate)?;
        }
        Ok(used)
    }
    fn issuer_available(&self, issuer: PublicKey) -> Result<usize, CoordinationError> {
        let own = self
            .state
            .limits
            .tenant_bytes
            .saturating_sub(self.tenant_used(issuer)?);
        if self.genesis.genesis.public.is_none() {
            return Ok(own);
        }
        Ok(own.min(
            self.state
                .limits
                .total_bytes
                .saturating_sub(self.total_used()?),
        ))
    }
    fn tenant_available(&self, scope: Scope) -> Result<usize, CoordinationError> {
        let issuer = self.issuer(scope).ok_or(CoordinationError::UnknownDomain)?;
        self.issuer_available(issuer)
    }
    fn usage(&self, key: PublicKey) -> Result<Usage, CoordinationError> {
        let tenant = self.tenant(key)?;
        let mut usage = Usage {
            bytes: self.tenant_used(key)?,
            registrations_today: if tenant.day == self.state.time / DAY {
                tenant.registrations
            } else {
                0
            },
            ..Usage::default()
        };
        for certificate in self.controls.iter().filter(|c| c.issuer == key) {
            let open = self
                .state
                .domains
                .iter()
                .any(|d| d.scope == certificate.control.scope && d.open());
            if open {
                usage.open_domains += 1;
            } else {
                usage.closed_domains += 1;
            }
        }
        Ok(usage)
    }
    fn admin_budget(
        &self,
        certificate: &ControlCertificate,
        retired: Option<Scope>,
    ) -> Result<(), CoordinationError> {
        let mut keys: BTreeSet<_> = certificate.control.admins.iter().copied().collect();
        for existing in self.controls.iter().filter(|c| {
            c.issuer == certificate.issuer
                && c.control.scope != certificate.control.scope
                && Some(c.control.scope) != retired
                && self.state.find(c.control.scope).is_ok_and(Domain::open)
        }) {
            keys.extend(existing.control.admins.iter().copied());
        }
        if keys.len() > MAX_ADMINS {
            return Err(CoordinationError::Backpressure);
        }
        Ok(())
    }
    fn control_authorized(
        &self,
        certificate: &ControlCertificate,
        time: u64,
    ) -> Result<(), CoordinationError> {
        self.admin_budget(certificate, None)?;
        let scope = certificate.control.scope;
        if self.genesis_bound(scope) || self.issuer(scope).is_none() {
            return certificate.verify(&self.genesis.genesis, time);
        }
        if !self.is_scope_authority(scope, certificate.issuer) {
            return Err(CoordinationError::Denied(Refusal::NotDomainAuthority));
        }
        certificate.check_without_authority(&self.genesis.genesis, time)
    }
    fn control(&mut self, certificate: ControlCertificate) -> Result<Outcome, CoordinationError> {
        let control = &certificate.control;
        let reference = certificate.reference()?;
        if let Some(index) = self
            .controls
            .iter()
            .position(|c| c.control.scope == control.scope)
        {
            let old = &self.controls[index];
            if control.revision
                != old
                    .control
                    .revision
                    .checked_add(1)
                    .ok_or(CoordinationError::Configuration)?
            {
                return Err(CoordinationError::Configuration);
            }
            let changes_authority =
                old.issuer != certificate.issuer || old.control.publisher != control.publisher;
            if changes_authority {
                return Err(CoordinationError::Denied(Refusal::NotDomainAuthority));
            }
            let growth = Self::certificate_bytes(&certificate)?
                .saturating_sub(Self::certificate_bytes(old)?);
            if growth > self.issuer_available(certificate.issuer)? {
                return Err(CoordinationError::Backpressure);
            }
            if !self.state.find(control.scope)?.open() {
                return Err(CoordinationError::Denied(Refusal::ClosedDomain));
            }
            self.controls[index] = certificate;
        } else {
            if control.revision != 1 || self.controls.len() >= 64 {
                return Err(CoordinationError::Configuration);
            }
            self.tenant_admits(certificate.issuer)?;
            self.install(certificate)?;
        }
        Ok(Outcome::Controlled(reference))
    }
    fn install(&mut self, certificate: ControlCertificate) -> Result<(), CoordinationError> {
        self.admin_budget(&certificate, None)?;
        let control = &certificate.control;
        self.state.domains.push(Domain::new(control.scope));
        self.controls.push(certificate);
        Ok(())
    }
    fn tenant_admits(&self, issuer: PublicKey) -> Result<(), CoordinationError> {
        let open = self
            .controls
            .iter()
            .filter(|c| c.issuer == issuer)
            .filter(|c| {
                self.state
                    .domains
                    .iter()
                    .any(|d| d.scope == c.control.scope && d.open())
            })
            .count();
        if open >= self.state.limits.tenant_domains {
            return Err(CoordinationError::Backpressure);
        }
        Ok(())
    }
    fn public_admits(
        &self,
        tenant: PublicKey,
        certificate: &ControlCertificate,
    ) -> Result<(), CoordinationError> {
        let public = self
            .genesis
            .genesis
            .public
            .as_ref()
            .ok_or(CoordinationError::Denied(Refusal::NoPublicService))?;
        let day = self.state.time / DAY;
        let mut cost = Self::certificate_bytes(certificate)?;
        match self.tenants.iter().find(|t| t.key == tenant) {
            Some(known) => {
                if known.day == day && known.registrations >= public.registrations_per_day {
                    return Err(CoordinationError::Backpressure);
                }
            }
            None => {
                if self.tenants.len() >= public.tenants as usize {
                    return Err(CoordinationError::Backpressure);
                }
                cost += ACCOUNTED_TENANT_BYTES;
            }
        }
        if self.controls.len() >= MAX_PUBLIC_DOMAINS || cost > self.issuer_available(tenant)? {
            return Err(CoordinationError::Backpressure);
        }
        self.tenant_admits(tenant)
    }
    fn count_registration(&mut self, tenant: PublicKey) {
        let day = self.state.time / DAY;
        match self.tenants.iter_mut().find(|t| t.key == tenant) {
            Some(known) if known.day == day => known.registrations += 1,
            Some(known) => {
                known.day = day;
                known.registrations = 1;
            }
            None => self.tenants.push(Tenant {
                key: tenant,
                day,
                registrations: 1,
            }),
        }
    }
    fn registration_authorized(
        &self,
        request: &Request,
        grant: &SignedCoordinationGrant,
        admission: Option<&SignedTenantAdmission>,
        certificate: &ControlCertificate,
        time: u64,
    ) -> Result<(), CoordinationError> {
        let public = self
            .genesis
            .genesis
            .public
            .as_ref()
            .ok_or(CoordinationError::Denied(Refusal::NoPublicService))?;
        let scope = certificate.control.scope;
        let app = grant
            .verify(time)
            .map_err(|_| CoordinationError::Denied(Refusal::InvalidGrant))?;
        let admitted = match (public.admission, admission) {
            (None, None) => true,
            (Some(provider), Some(admission)) => admission
                .verify(provider, time)
                .is_ok_and(|tenant| tenant.0 == request.key),
            _ => false,
        };
        if !admitted {
            return Err(CoordinationError::Denied(Refusal::NotAdmitted));
        }
        if certificate.issuer != request.key
            || grant.grant.tenant.0 != request.key
            || scope.app != app
            || certificate
                .control
                .publisher
                .as_deref()
                .is_some_and(|publisher| *publisher != grant.grant.publisher.0)
        {
            return Err(CoordinationError::Denied(Refusal::InvalidGrant));
        }
        if certificate.control.revision != 1 {
            return Err(CoordinationError::Denied(Refusal::InvalidControl));
        }
        if self.genesis_bound(scope) {
            return Err(CoordinationError::Denied(Refusal::NotDomainAuthority));
        }
        self.admin_budget(certificate, None)?;
        certificate.check_without_authority(&self.genesis.genesis, time)
    }
    fn current_control(
        &self,
        scope: Scope,
        time: u64,
    ) -> Result<&DomainControl, CoordinationError> {
        let certificate = self
            .controls
            .iter()
            .find(|c| c.control.scope == scope)
            .ok_or(CoordinationError::UnknownDomain)?;
        if self.genesis_bound(scope) {
            certificate.verify(&self.genesis.genesis, time)?;
        } else {
            certificate.check_without_authority(&self.genesis.genesis, time)?;
        }
        Ok(&certificate.control)
    }
    fn calls_as_admin(&self, control: &DomainControl, request: &Request, time: u64) -> bool {
        if let Some(publisher) = control.publisher.as_deref().copied() {
            let expected = rocky_credentials::DatabaseContext {
                publisher,
                scope: control.scope,
            };
            if request
                .certificate
                .as_ref()
                .and_then(|certificate| certificate.capability.extended().ok())
                .and_then(|extended| extended.database)
                != Some(expected)
            {
                return false;
            }
        }
        request.identity(time).is_some_and(|identity| {
            control.admins.contains(&identity)
                && self.state.find(control.scope).is_ok_and(|domain| {
                    !domain.revocations.revokes_origin(&identity)
                        && !domain.revocations.ends_node_of(identity, request.key, true)
                })
        })
    }
    fn closure_authorized(&self, request: &Request, scope: Scope) -> Result<(), CoordinationError> {
        if !self.is_scope_authority(scope, request.key) || self.issuer(scope) != Some(request.key) {
            return Err(CoordinationError::Denied(Refusal::NotDomainAuthority));
        }
        Ok(())
    }
    fn transition(&mut self, request: &Request) -> Result<Outcome, CoordinationError> {
        let time = self.state.time;
        match &request.operation {
            Operation::Control(certificate) => {
                if certificate.issuer != request.key {
                    return Err(CoordinationError::Denied(Refusal::NotDomainAuthority));
                }
                self.control_authorized(certificate, time)?;
                self.control((**certificate).clone())
            }
            Operation::Register(registration) => {
                let Registration {
                    grant,
                    admission,
                    certificate,
                } = &**registration;
                self.registration_authorized(
                    request,
                    grant,
                    admission.as_ref(),
                    certificate,
                    time,
                )?;
                let scope = certificate.control.scope;
                if let Some(known) = self.controls.iter().find(|c| c.control.scope == scope) {
                    let still_open = self.state.find(scope).is_ok_and(Domain::open);
                    let same_registration_retried = known == certificate && still_open;
                    return if same_registration_retried {
                        certificate.reference().map(Outcome::Controlled)
                    } else {
                        Err(CoordinationError::Denied(Refusal::AlreadyRegistered))
                    };
                }
                self.public_admits(request.key, certificate)?;
                self.count_registration(request.key);
                self.install(certificate.clone())?;
                certificate.reference().map(Outcome::Controlled)
            }
            Operation::Usage => self.usage(request.key).map(Outcome::Usage),
            Operation::Claim {
                scope,
                counter,
                claim,
            } => self.claim(*scope, counter, *claim, request),
            Operation::Counter {
                scope,
                counter,
                claim,
            } => self.counter(*scope, counter, *claim, request),
            Operation::Revoke { scope, revocations } => self.revoke(*scope, revocations),
            Operation::Close { scope } => {
                self.closure_authorized(request, *scope)?;
                self.state.close(*scope).map(|()| Outcome::Closed)
            }
            Operation::Membership {
                voters,
                expected,
                expected_revision,
            } => {
                if request.key != self.genesis.genesis.membership_authority {
                    return Err(CoordinationError::Denied(Refusal::NotMembershipAuthority));
                }
                if voters.len() < 2
                    || !voters
                        .iter()
                        .all(|id| self.genesis.genesis.voters.iter().any(|v| v.id == *id))
                {
                    return Err(CoordinationError::Denied(Refusal::UnknownVoter));
                }
                if expected != &self.desired_voters
                    || *expected_revision != self.membership_revision
                {
                    return Err(CoordinationError::Configuration);
                }
                self.membership_revision = self
                    .membership_revision
                    .checked_add(1)
                    .ok_or(CoordinationError::Configuration)?;
                self.desired_voters = voters.clone();
                Ok(Outcome::Membership(voters.clone()))
            }
        }
    }
}

#[cfg(test)]
#[path = "machine/public_tests.rs"]
mod public_tests;
#[cfg(test)]
#[path = "machine/retention_tests.rs"]
mod retention_tests;

#[cfg(test)]
#[path = "machine/counter_tests.rs"]
mod counter_tests;
#[cfg(test)]
#[path = "machine/fixture.rs"]
mod fixture;
#[cfg(test)]
#[path = "machine/member_tests.rs"]
mod member_tests;
