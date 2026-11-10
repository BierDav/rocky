use crate::{certified::*, retention::DAY, *};
use p2panda_core::{PublicKey, Signature};
use rocky_credentials::{
    CredentialChain, RevocationSet, SignedCoordinationGrant, SignedTenantAdmission,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Accounting overhead of a domain beside its control certificate and sessions.
const DOMAIN_BYTES: usize = 1024;
/// Accounting of one tenant record.
const TENANT_BYTES: usize = 64;
/// Domains of a public service, open and closed.
pub const MAX_PUBLIC_DOMAINS: usize = 65536;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Domain {
    scope: Scope,
    coordinated: coordinated::Logs,
    counters: counters::Counters,
    /// Statements of the domain's publisher that end claim rights,
    /// as nodes pushed them, and their encoded size.
    revocations: RevocationSet,
    revocation_bytes: usize,
    /// Closed by its owner: the identity never reopens.
    closed: bool,
}
impl Domain {
    fn new(scope: Scope, configuration: Configuration) -> Self {
        Self {
            scope,
            coordinated: coordinated::Logs::new(scope, configuration),
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
        self.coordinated.bytes() + self.counters.bytes() + self.revocation_bytes
    }
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct State {
    domains: Vec<Domain>,
    limits: Limits,
    /// Authority time in seconds; it never moves backwards.
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
    /// Advances authority time, clamping a backward clock.
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
    /// Owner-initiated closure. A closed identity never reopens; an entry
    /// still pending must be decided first.
    fn close(&mut self, scope: Scope) -> Result<(), CoordinationError> {
        let domain = self.domain(scope)?;
        if domain.coordinated.pending().next().is_some() {
            return Err(CoordinationError::Denied);
        }
        domain.closed = true;
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Operation {
    Control(ControlCertificate),
    Coordinated {
        log: TipLog,
        action: crate::coordinated::Action,
    },
    /// The domain's default minimum in-sync size, by the scope's authority
    /// before the first join.
    Configure {
        scope: Scope,
        minimum: usize,
    },
    Membership {
        voters: BTreeSet<u64>,
        expected: BTreeSet<u64>,
        expected_revision: u64,
    },
    /// Owner-initiated closure, by the scope's authority.
    Close {
        scope: Scope,
    },
    /// A tenant installs a domain of an application its publisher granted
    /// it, under its own control key. Public services only.
    Register(Box<Registration>),
    /// The signing tenant's quota use.
    Usage,
    /// Takes the next value of `scope`'s counter, or the value `claim`
    /// already took. By a trusted node of the open domain, or a node whose
    /// claim right covers the counter.
    Claim {
        scope: Scope,
        counter: crate::counters::Name,
        claim: crate::counters::Claim,
    },
    /// A counter's last value and, for `claim`, what it took. By a trusted
    /// node of the domain, or a holder of a claim right that covers it.
    Counter {
        scope: Scope,
        counter: crate::counters::Name,
        claim: Option<crate::counters::Claim>,
    },
    /// Revocations of the domain's publisher, which end claim rights from
    /// now on. Statements verify themselves, so any node may push them.
    Revoke {
        scope: Scope,
        revocations: RevocationSet,
    },
}
/// What a tenant presents to register a domain.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Registration {
    pub grant: SignedCoordinationGrant,
    pub admission: Option<SignedTenantAdmission>,
    pub certificate: ControlCertificate,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Request {
    pub genesis: Commitment,
    pub key: PublicKey,
    /// The signing node's certificate, by which a trusted identity
    /// vouches for it.
    pub certificate: Option<rocky_credentials::SignedCapability>,
    /// The node's claim chain from the domain's publisher, on a counter
    /// claim or read by a node that is not trusted.
    pub claims: Option<CredentialChain>,
    pub id: Commitment,
    pub time: u64,
    pub operation: Operation,
    signature: Signature,
}
/// A node certificate its node signed itself: its key is its own identity,
/// as controls that name node keys trust it.
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
    use rocky_credentials::{AuthorRange, Capability, Extended, Extension, PeerId};
    let capability = Capability {
        subject: key.public_key(),
        kind: rocky_core::PrincipalKind::Node,
        grants: Vec::new(),
        not_before: 0,
        expires: rocky_credentials::NEVER,
        delegations: 0,
        extension: Extension::new(Extended {
            node: Some(PeerId::of(key.public_key())),
            authors: Some(AuthorRange::new(1, 1).expect("author range")),
            database: Some(database),
            sequence: Some(0),
            ..Extended::default()
        })
        .expect("extension"),
    };
    rocky_credentials::SignedCapability::sign(capability, key).expect("certificate")
}
/// The one tip unit tests coordinate in `scope`.
#[cfg(test)]
pub(crate) fn tip_log(scope: Scope) -> TipLog {
    TipLog {
        scope,
        tip: TipId([9; 32]),
    }
}
/// A request of a node that presents its own certificate.
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
/// A Raft log entry: the leader-proposed authority time and a client request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Proposal {
    pub time: u64,
    pub request: Request,
}
impl Request {
    pub fn sign(
        genesis: Commitment,
        id: Commitment,
        operation: Operation,
        key: &p2panda_core::PrivateKey,
    ) -> Result<Self, CoordinationError> {
        Self::sign_at(genesis, id, operation, key, now()?)
    }
    pub fn sign_at(
        genesis: Commitment,
        id: Commitment,
        operation: Operation,
        key: &p2panda_core::PrivateKey,
        time: u64,
    ) -> Result<Self, CoordinationError> {
        Self::certified(genesis, id, operation, (key, None), time)
    }
    /// A request of a node presenting `certificate`, its identity's.
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
        let mut request = Self {
            genesis,
            key: key.public_key(),
            certificate,
            claims: None,
            id,
            time,
            operation,
            signature: key.sign(b""),
        };
        request.signature = key.sign(&request.signing_bytes()?);
        Ok(request)
    }
    /// This request presenting the claim chain `claims`, signed again by `key`.
    pub fn with_claims(
        mut self,
        claims: CredentialChain,
        key: &p2panda_core::PrivateKey,
    ) -> Result<Self, CoordinationError> {
        if key.public_key() != self.key {
            return Err(CoordinationError::Denied);
        }
        self.claims = Some(claims);
        self.signature = key.sign(&self.signing_bytes()?);
        Ok(self)
    }
    fn signing_bytes(&self) -> Result<Vec<u8>, CoordinationError> {
        bytes(
            "rocky.coordination-request.v7",
            &(
                self.genesis,
                self.key,
                &self.certificate,
                &self.claims,
                self.id,
                self.time,
                &self.operation,
            ),
        )
    }
    /// The identity whose current certificate the signing node presents.
    pub fn identity(&self, time: u64) -> Option<PublicKey> {
        self.certificate.as_ref()?.certifies(self.key, time).ok()
    }
    pub fn authenticate(&self, genesis: Commitment) -> Result<(), CoordinationError> {
        if self.genesis != genesis || !self.key.verify(&self.signing_bytes()?, &self.signature) {
            return Err(CoordinationError::Denied);
        }
        Ok(())
    }
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
    /// Administrative mutations keep retry outcomes. Reads keep nothing, log
    /// actions deduplicate by their own sequences, claims by their own
    /// identities and revocations by their statements.
    fn retained(&self) -> bool {
        !matches!(
            self.operation,
            Operation::Coordinated { .. }
                | Operation::Usage
                | Operation::Claim { .. }
                | Operation::Counter { .. }
                | Operation::Revoke { .. }
        ) || matches!(
            self.operation,
            Operation::Coordinated {
                action: crate::coordinated::Action::Configure { .. },
                ..
            }
        )
    }
    /// Answered from applied state after a linearizable read barrier.
    pub fn read_only(&self) -> bool {
        matches!(
            self.operation,
            Operation::Usage
                | Operation::Counter { .. }
                | Operation::Coordinated {
                    action: crate::coordinated::Action::Head
                        | crate::coordinated::Action::Status { .. },
                    ..
                }
        )
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Outcome {
    CoordinatedHead(Box<crate::coordinated::Head>),
    CoordinatedStatus(rocky_core::LogStatus),
    Coordinated(crate::coordinated::Result),
    Claimed(u64),
    Counter(crate::counters::Reading),
    /// The digest of the domain's live revocations after a push.
    Revoked(Commitment),
    Controlled(Commitment),
    Membership(BTreeSet<u64>),
    Closed,
    Usage(Usage),
}
/// A tenant's use of its budgets.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Usage {
    pub open_domains: usize,
    pub closed_domains: usize,
    pub bytes: usize,
    pub registrations_today: u32,
}
/// A mutation's outcome, kept for retries of the same signer/ID within the
/// retry window. After it, the signed request is too old to apply again.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Retained {
    key: PublicKey,
    id: Commitment,
    time: u64,
    digest: Commitment,
    result: Result<Outcome, CoordinationError>,
    /// The tenant whose retry budget holds it, on a public service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tenant: Option<PublicKey>,
}
/// A tenant of a public service: its control key and registration churn.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Tenant {
    key: PublicKey,
    /// The day of `registrations`, in authority time.
    day: u64,
    registrations: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Metadata {
    pub genesis: CertifiedGenesis,
    controls: Vec<ControlCertificate>,
    state: State,
    retained: Vec<Retained>,
    pub desired_voters: BTreeSet<u64>,
    membership_revision: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tenants: Vec<Tenant>,
}
impl Metadata {
    #[cfg(test)]
    pub fn new(genesis: CertifiedGenesis) -> Result<Self, CoordinationError> {
        Self::with_limits(genesis, Limits::default())
    }
    pub fn with_limits(
        genesis: CertifiedGenesis,
        limits: Limits,
    ) -> Result<Self, CoordinationError> {
        limits.validate()?;
        let mut result = Self {
            desired_voters: genesis.genesis.initial_voters.clone(),
            membership_revision: 0,
            genesis,
            controls: Vec::new(),
            state: State::new(limits),
            retained: Vec::new(),
            tenants: Vec::new(),
        };
        if let Some(directory) = result.genesis.genesis.directory.clone() {
            result.control(directory)?;
        }
        Ok(result)
    }
    pub fn time(&self) -> u64 {
        self.state.time
    }
    pub fn tenants(&self) -> usize {
        self.tenants.len()
    }
    pub fn retention(&self) -> Retention {
        self.state.retention()
    }
    pub fn retry_records(&self) -> usize {
        self.retained.len()
    }
    pub fn preflight(&self, request: &Request) -> Result<(), CoordinationError> {
        request.authenticate(self.genesis.id()?)?;
        request.fresh()?;
        if self
            .retained
            .iter()
            .any(|r| r.key == request.key && r.id == request.id)
        {
            return Ok(());
        }
        let time = request.time;
        match &request.operation {
            Operation::Coordinated { log, action } => {
                self.log_authorized(*log, action, request, time)
            }
            Operation::Configure { scope, .. } => {
                self.closure_authorized(request, *scope)?;
                self.state
                    .find(*scope)?
                    .open()
                    .then_some(())
                    .ok_or(CoordinationError::Denied)
            }
            Operation::Control(c) => {
                if c.issuer != request.key {
                    return Err(CoordinationError::Denied);
                }
                self.control_authorized(c, time)
            }
            Operation::Membership { .. } => {
                if request.key == self.genesis.genesis.membership_authority {
                    Ok(())
                } else {
                    Err(CoordinationError::Denied)
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
            Operation::Claim { scope, counter, .. } => {
                self.counter_authorized(*scope, counter, true, request, time)
            }
            Operation::Counter { scope, counter, .. } => {
                self.counter_authorized(*scope, counter, false, request, time)
            }
            Operation::Revoke { scope, revocations } => self
                .merged_revocations(*scope, revocations, time)
                .map(|_| ()),
        }
    }
    /// Advances authority time to the leader-proposed `time`, clamping a
    /// backward clock, then applies `request` deterministically.
    pub fn apply_at(&mut self, time: u64, request: &Request) -> Result<Outcome, CoordinationError> {
        if time > self.state.time {
            self.state.advance(time);
            let window = self.state.limits.retry_window;
            self.retained
                .retain(|r| r.time.saturating_add(window) >= time);
        }
        request.authenticate(self.genesis.id()?)?;
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
        // Its retry record may be gone: an old signed request never applies again.
        if request.time.saturating_add(self.state.limits.retry_window) < self.state.time {
            return Err(CoordinationError::Expired);
        }
        let tenant = self.retry_tenant(request);
        if request.retained()
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
        // A refused registration changes nothing and holds no tenant budget.
        let unregistered = matches!(request.operation, Operation::Register(_)) && result.is_err();
        if request.retained() && !unregistered {
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
    /// Answers a read-only request from applied state at its authority time.
    pub fn read(&self, request: &Request) -> Result<Outcome, CoordinationError> {
        request.authenticate(self.genesis.id()?)?;
        let time = self.state.time;
        match &request.operation {
            Operation::Coordinated {
                log,
                action: crate::coordinated::Action::Head,
            } => {
                self.log_authorized(*log, &crate::coordinated::Action::Head, request, time)?;
                Ok(Outcome::CoordinatedHead(Box::new(self.log_head(*log)?)))
            }
            Operation::Coordinated {
                log,
                action: crate::coordinated::Action::Status { position, hash },
            } => {
                self.log_authorized(
                    *log,
                    &crate::coordinated::Action::Status {
                        position: *position,
                        hash: *hash,
                    },
                    request,
                    time,
                )?;
                Ok(Outcome::CoordinatedStatus(
                    self.log_status(*log, *position, *hash)?,
                ))
            }
            Operation::Usage => self.usage(request.key).map(Outcome::Usage),
            Operation::Counter {
                scope,
                counter,
                claim,
            } => self.counter(*scope, counter, *claim, request),
            _ => Err(CoordinationError::Denied),
        }
    }
    /// A trusted node of `scope` under its current control, or a node
    /// whose claim right covers `counter`; a claim also needs the domain
    /// open. A claim right never reaches beyond counters.
    fn counter_authorized(
        &self,
        scope: Scope,
        counter: &crate::counters::Name,
        claim: bool,
        request: &Request,
        time: u64,
    ) -> Result<(), CoordinationError> {
        let control = self.current_control(scope, time)?;
        if !self.trusts(control, request, time)
            && !self.holds_claim(control, counter, request, time)
        {
            return Err(CoordinationError::Denied);
        }
        if claim && !self.state.find(scope)?.open() {
            return Err(CoordinationError::Denied);
        }
        Ok(())
    }
    /// Whether the request's claim chain, rooted in the publisher `control`
    /// names, lets its node claim `counter` at `time` under the
    /// revocations the domain holds.
    fn holds_claim(
        &self,
        control: &DomainControl,
        counter: &crate::counters::Name,
        request: &Request,
        time: u64,
    ) -> bool {
        let (Some(publisher), Some(chain), Ok(domain)) = (
            control.publisher.as_deref().copied(),
            request.claims.as_ref(),
            self.state.find(control.scope),
        ) else {
            return false;
        };
        if chain
            .verify_database(rocky_credentials::DatabaseContext {
                publisher,
                scope: control.scope,
            })
            .is_err()
        {
            return false;
        }
        chain
            .claimant(
                publisher,
                request.key,
                (control.scope, counter.as_bytes()),
                &domain.revocations,
                time,
            )
            .is_ok()
    }
    /// The domain's revocations with `pushed` merged: statements its
    /// control's publisher, or an issuer under it, signed for its application.
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
            .ok_or(CoordinationError::Denied)?;
        let domain = self.state.find(scope)?;
        if !domain.open() {
            return Err(CoordinationError::Denied);
        }
        pushed
            .verify_in(rocky_credentials::DatabaseContext { publisher, scope })
            .map_err(|_| CoordinationError::Denied)?;
        let mut merged = domain.revocations.clone();
        merged
            .merge(pushed, publisher, scope.app, time)
            .map_err(|_| CoordinationError::Denied)?;
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
        self.counter_authorized(scope, counter, false, request, time)?;
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
        self.counter_authorized(scope, counter, true, request, time)?;
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
            .ok_or(CoordinationError::Denied)
    }
    fn genesis_bound(&self, scope: Scope) -> bool {
        self.genesis
            .genesis
            .authorities
            .iter()
            .any(|a| a.scope == scope)
    }
    /// Whether `key` controls `scope`: its root-bound issuer, or the tenant
    /// that registered it.
    fn authority(&self, scope: Scope, key: PublicKey) -> bool {
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
    /// The request's scope owns its budget, even when tenants share a device key.
    pub(crate) fn retry_tenant(&self, request: &Request) -> Option<PublicKey> {
        self.genesis.genesis.public.as_ref()?;
        let (scope, counter) = match &request.operation {
            Operation::Register(_) | Operation::Usage => return Some(request.key),
            Operation::Membership { .. } => return None,
            Operation::Control(certificate) => (certificate.control.scope, None),
            Operation::Close { scope }
            | Operation::Configure { scope, .. }
            | Operation::Revoke { scope, .. } => (*scope, None),
            Operation::Coordinated { log, .. } => (log.scope, None),
            Operation::Claim { scope, counter, .. } | Operation::Counter { scope, counter, .. } => {
                (*scope, Some(counter))
            }
        };
        let certificate = self.controls.iter().find(|c| c.control.scope == scope)?;
        if certificate.issuer != request.key
            && !self.trusts(&certificate.control, request, request.time)
            && !counter.is_some_and(|counter| {
                self.holds_claim(&certificate.control, counter, request, request.time)
            })
        {
            return None;
        }
        self.tenant(certificate.issuer).ok().map(|t| t.key)
    }
    /// After authorization, unknown accounts still share the unregistered lane.
    pub(crate) fn tenant_lane(&self, request: &Request) -> Option<PublicKey> {
        self.tenant(self.retry_tenant(request)?).ok().map(|t| t.key)
    }
    fn certificate_bytes(certificate: &ControlCertificate) -> Result<usize, CoordinationError> {
        Ok(DOMAIN_BYTES + encode(certificate)?.len())
    }
    /// Accounted bytes of the controls `issuer` signed and their sessions.
    fn tenant_used(&self, issuer: PublicKey) -> Result<usize, CoordinationError> {
        let mut used = usize::from(self.tenants.iter().any(|t| t.key == issuer)) * TENANT_BYTES;
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
    /// Accounted bytes of every control, session and tenant.
    pub(crate) fn total_used(&self) -> Result<usize, CoordinationError> {
        let mut used = self.state.bytes() + self.tenants.len() * TENANT_BYTES;
        for certificate in &self.controls {
            used += Self::certificate_bytes(certificate)?;
        }
        Ok(used)
    }
    /// Bytes `issuer` may still add: within its own budget and, on a public
    /// service, within the service's.
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
    /// Accounted bytes the tenant issuing `scope`'s control may still add.
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
    fn trusted_budget(
        &self,
        certificate: &ControlCertificate,
        retired: Option<Scope>,
    ) -> Result<(), CoordinationError> {
        let mut keys: BTreeSet<_> = certificate.control.trusted.iter().copied().collect();
        for existing in self.controls.iter().filter(|c| {
            c.issuer == certificate.issuer
                && c.control.scope != certificate.control.scope
                && Some(c.control.scope) != retired
                && self.state.find(c.control.scope).is_ok_and(Domain::open)
        }) {
            keys.extend(existing.control.trusted.iter().copied());
        }
        if keys.len() > crate::coordinated::TRUSTED_NODES {
            return Err(CoordinationError::Backpressure);
        }
        Ok(())
    }
    /// A root-bound control, or a revision by the scope's authority.
    fn control_authorized(
        &self,
        certificate: &ControlCertificate,
        time: u64,
    ) -> Result<(), CoordinationError> {
        self.trusted_budget(certificate, None)?;
        let scope = certificate.control.scope;
        if self.genesis_bound(scope) || self.issuer(scope).is_none() {
            return certificate.verify(&self.genesis.genesis, time);
        }
        if !self.authority(scope, certificate.issuer) {
            return Err(CoordinationError::Denied);
        }
        certificate.check(&self.genesis.genesis, time)
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
            // Authority changes are a migration, not a control revision.
            if old.issuer != certificate.issuer || old.control.publisher != control.publisher {
                return Err(CoordinationError::Denied);
            }
            let logs = &self.state.find(control.scope)?.coordinated;
            if logs.pending().next().is_some()
                || logs
                    .members()
                    .any(|m| !control.trusted.contains(&m.identity))
            {
                return Err(CoordinationError::Backpressure);
            }
            let growth = Self::certificate_bytes(&certificate)?
                .saturating_sub(Self::certificate_bytes(old)?);
            if growth > self.issuer_available(certificate.issuer)? {
                return Err(CoordinationError::Backpressure);
            }
            if !self.state.find(control.scope)?.open() {
                return Err(CoordinationError::Denied);
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
        self.trusted_budget(&certificate, None)?;
        let control = &certificate.control;
        self.state
            .domains
            .push(Domain::new(control.scope, control.configuration));
        self.controls.push(certificate);
        Ok(())
    }
    /// A tenant's open domains are bounded; closed ones keep only a marker.
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
    /// Whether a public tenant may add `certificate` now: domain count,
    /// bytes, daily registrations and the service's tenant and domain caps.
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
            .ok_or(CoordinationError::Denied)?;
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
                cost += TENANT_BYTES;
            }
        }
        if self.controls.len() >= MAX_PUBLIC_DOMAINS || cost > self.issuer_available(tenant)? {
            return Err(CoordinationError::Backpressure);
        }
        self.tenant_admits(tenant)
    }
    /// Counts a registration against the tenant's day, adding the tenant.
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
            .ok_or(CoordinationError::Denied)?;
        let scope = certificate.control.scope;
        let app = grant.verify(time).map_err(|_| CoordinationError::Denied)?;
        let admitted = match (public.admission, admission) {
            (None, None) => true,
            (Some(provider), Some(admission)) => admission
                .verify(provider, time)
                .is_ok_and(|tenant| tenant.0 == request.key),
            _ => false,
        };
        if !admitted
            || certificate.issuer != request.key
            || grant.grant.tenant.0 != request.key
            || scope.app != app
            || certificate
                .control
                .publisher
                .as_deref()
                .is_some_and(|publisher| *publisher != grant.grant.publisher.0)
            || certificate.control.revision != 1
            || self.genesis_bound(scope)
        {
            return Err(CoordinationError::Denied);
        }
        self.trusted_budget(certificate, None)?;
        certificate.check(&self.genesis.genesis, time)
    }
    /// `scope`'s control, valid at `time`.
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
            certificate.check(&self.genesis.genesis, time)?;
        }
        Ok(&certificate.control)
    }
    /// Whether the request's node certificate is a trusted identity's.
    fn trusts(&self, control: &DomainControl, request: &Request, time: u64) -> bool {
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
            control.trusted.contains(&identity)
                && self.state.find(control.scope).is_ok_and(|domain| {
                    !domain.revocations.revokes_origin(&identity)
                        && !domain.revocations.ends_node_of(identity, request.key, true)
                })
        })
    }
    /// A trusted node of `scope` under its current control.
    fn authorized(
        &self,
        scope: Scope,
        request: &Request,
        time: u64,
    ) -> Result<&DomainControl, CoordinationError> {
        let control = self.current_control(scope, time)?;
        if !self.trusts(control, request, time) {
            return Err(CoordinationError::Denied);
        }
        Ok(control)
    }
    fn closure_authorized(&self, request: &Request, scope: Scope) -> Result<(), CoordinationError> {
        if !self.authority(scope, request.key) || self.issuer(scope) != Some(request.key) {
            return Err(CoordinationError::Denied);
        }
        Ok(())
    }
    fn log_authorized(
        &self,
        log: TipLog,
        action: &crate::coordinated::Action,
        request: &Request,
        time: u64,
    ) -> Result<(), CoordinationError> {
        use crate::coordinated::Action;
        let scope = log.scope;
        match action {
            // The authority or a trusted node relays a tip's opening, seal
            // or bookmark move.
            Action::Configure { .. } | Action::Seal { .. } => self
                .closure_authorized(request, scope)
                .or_else(|_| self.authorized(scope, request, time).map(|_| ())),
            // A trusted node or the authority submits; the authority signs.
            Action::Recover { .. } => {
                let owner = self.issuer(scope).ok_or(CoordinationError::UnknownDomain)?;
                if !self.authority(scope, owner) {
                    return Err(CoordinationError::Denied);
                }
                self.closure_authorized(request, scope)
                    .or_else(|_| self.authorized(scope, request, time).map(|_| ()))
            }
            Action::Resolve { .. } => {
                if self
                    .genesis
                    .genesis
                    .voters
                    .iter()
                    .any(|v| v.key == request.key)
                {
                    Ok(())
                } else {
                    Err(CoordinationError::Denied)
                }
            }
            _ => self.authorized(scope, request, time).map(|_| ()),
        }?;
        if !matches!(action, Action::Head | Action::Status { .. })
            && !self.state.find(scope)?.open()
        {
            return Err(CoordinationError::Denied);
        }
        Ok(())
    }
    pub(crate) fn pending_logs(&self) -> Vec<(TipLog, crate::coordinated::Pending)> {
        self.state
            .domains
            .iter()
            .filter(|d| d.open())
            .flat_map(|d| {
                d.coordinated.pending().map(|(tip, p)| {
                    let log = TipLog {
                        scope: d.scope,
                        tip,
                    };
                    (log, p.clone())
                })
            })
            .collect()
    }
    pub(crate) fn log_head(
        &self,
        log: TipLog,
    ) -> Result<crate::coordinated::Head, CoordinationError> {
        let domain = self.state.find(log.scope)?;
        let mut head = domain.coordinated.head(log.tip);
        head.closed = !domain.open();
        Ok(head)
    }
    pub(crate) fn log_status(
        &self,
        log: TipLog,
        position: u64,
        hash: Commitment,
    ) -> Result<rocky_core::LogStatus, CoordinationError> {
        Ok(self
            .state
            .find(log.scope)?
            .coordinated
            .status(log.tip, position, hash))
    }
    fn coordinated(
        &mut self,
        log: TipLog,
        action: &crate::coordinated::Action,
        request: &Request,
    ) -> Result<Outcome, CoordinationError> {
        use crate::coordinated::Action;
        let scope = log.scope;
        self.log_authorized(log, action, request, self.state.time)?;
        if matches!(action, Action::Head) {
            return self
                .log_head(log)
                .map(Box::new)
                .map(Outcome::CoordinatedHead);
        }
        if let Action::Status { position, hash } = action {
            return self
                .log_status(log, *position, *hash)
                .map(Outcome::CoordinatedStatus);
        }
        let domain = self.state.find(scope)?;
        let mut next = domain.coordinated.working(request.key, log.tip, action)?;
        let before = domain.coordinated.get(log.tip).map_or(0, |l| l.bytes());
        let result = match action {
            Action::Resolve {
                pending,
                acknowledgments,
            } => {
                if acknowledgments.len() > crate::coordinated::TRUSTED_NODES {
                    return Err(CoordinationError::Denied);
                }
                let mut keys = Vec::new();
                for ack in acknowledgments {
                    ack.authenticate(self.genesis.id()?)?;
                    self.authorized(scope, ack, ack.time)?;
                    if ack.operation
                        != (Operation::Coordinated {
                            log,
                            action: Action::Acknowledge {
                                pending: pending.clone(),
                            },
                        })
                        || ack.time > pending.deadline
                        || !next.head.members.iter().any(|m| m.key == ack.key)
                        || keys.contains(&ack.key)
                    {
                        return Err(CoordinationError::Denied);
                    }
                    keys.push(ack.key);
                }
                next.resolve(pending, &keys, self.state.time)
            }
            Action::Append { .. } => {
                let minute = self.state.time / 60;
                let issuer = self.issuer(scope).ok_or(CoordinationError::UnknownDomain)?;
                let appends: u64 = self
                    .state
                    .domains
                    .iter()
                    .filter(|d| self.issuer(d.scope) == Some(issuer))
                    .map(|d| d.coordinated.appends_in(minute))
                    .sum();
                next.apply(
                    request.key,
                    action,
                    self.state.time,
                    appends < u64::from(crate::coordinated::APPENDS_PER_MINUTE),
                )
            }
            Action::Seal { terminal } => next.seal(request.key, *terminal),
            Action::Recover { seal } => {
                let owner = self.issuer(scope).ok_or(CoordinationError::UnknownDomain)?;
                next.recover(log, seal, owner)
            }
            Action::AcknowledgeTerminal { acknowledgment } => {
                next.acknowledge_terminal(log, request.key, acknowledgment)
            }
            Action::Join { .. } => {
                let joined = next.apply(request.key, action, self.state.time, true);
                // The in-sync set counts nodes and remembers whose they are.
                let identity = request
                    .identity(self.state.time)
                    .ok_or(CoordinationError::Denied)?;
                if let Some(member) = next.head.members.iter_mut().find(|m| m.key == request.key) {
                    member.identity = identity;
                }
                joined
            }
            _ => next.apply(request.key, action, self.state.time, true),
        };
        let growth = next.bytes().saturating_sub(before);
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
        let logs = &mut self.state.domain(scope)?.coordinated;
        logs.store(log.tip, next);
        if matches!(action, Action::Join { .. }) && result.is_ok() {
            logs.leave_others(request.key, log.tip);
        }
        result.map(Outcome::Coordinated)
    }
    fn transition(&mut self, request: &Request) -> Result<Outcome, CoordinationError> {
        let time = self.state.time;
        match &request.operation {
            Operation::Coordinated { log, action } => self.coordinated(*log, action, request),
            Operation::Configure { scope, minimum } => {
                self.closure_authorized(request, *scope)?;
                let domain = self.state.domain(*scope)?;
                if !domain.open() {
                    return Err(CoordinationError::Denied);
                }
                domain
                    .coordinated
                    .configure(*minimum)
                    .map(Outcome::Coordinated)
            }
            Operation::Control(certificate) => {
                if certificate.issuer != request.key {
                    return Err(CoordinationError::Denied);
                }
                self.control_authorized(certificate, time)?;
                self.control(certificate.clone())
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
                    // A registered identity keeps its authority; a closed one is never reused.
                    let open = self.state.find(scope).is_ok_and(Domain::open);
                    return if known == certificate && open {
                        certificate.reference().map(Outcome::Controlled)
                    } else {
                        Err(CoordinationError::Denied)
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
                if request.key != self.genesis.genesis.membership_authority
                    || voters.len() < 2
                    || !voters
                        .iter()
                        .all(|id| self.genesis.genesis.voters.iter().any(|v| v.id == *id))
                {
                    return Err(CoordinationError::Denied);
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
#[path = "machine/claim_tests.rs"]
mod claim_tests;
#[cfg(test)]
#[path = "machine/coordinated_tests.rs"]
mod coordinated_tests;
#[cfg(test)]
#[path = "machine/counter_tests.rs"]
mod counter_tests;
#[cfg(test)]
#[path = "machine/recovery_tests.rs"]
mod recovery_tests;
#[cfg(test)]
#[path = "machine/tip_tests.rs"]
mod tip_tests;
