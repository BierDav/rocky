use crate::{certified::*, retention::DAY, *};
use p2panda_core::{PublicKey, Signature};
use rocky_credentials::{SignedCoordinationGrant, SignedTenantAdmission};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Accounting overhead of a domain beside its control certificate and sessions.
const DOMAIN_BYTES: usize = 1024;
/// Accounting of one tenant record.
const TENANT_BYTES: usize = 64;
/// Domains of a public service, open and closed.
pub const MAX_PUBLIC_DOMAINS: usize = 4096;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Operation {
    Control(ControlCertificate),
    Coordinated {
        scope: Scope,
        action: crate::coordinated::Action,
    },
    Acquire {
        scope: Scope,
    },
    Takeover {
        scope: Scope,
        expected_epoch: u64,
    },
    Replace {
        record: ReplacementRecord,
        certificate: ControlCertificate,
    },
    Head {
        scope: Scope,
    },
    Confirm(Confirmation),
    Lookup {
        scope: Scope,
        request: RequestId,
    },
    Membership {
        voters: BTreeSet<u64>,
        expected: BTreeSet<u64>,
        expected_revision: u64,
    },
    /// The signer's durable acknowledgment of its identities through `through`.
    Retire {
        scope: Scope,
        through: u64,
    },
    /// Owner-initiated closure at an exact head, by the scope's authority.
    Close {
        head: StrongHead,
    },
    /// A tenant installs a domain of an application its publisher granted
    /// it, under its own control key. Public services only.
    Register(Box<Registration>),
    /// The signing tenant's quota use.
    Usage,
    /// The scope authority's move to the next definition generation at an
    /// exact head; see `rocky_core::Activation`.
    Activate(Activation),
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
    pub id: Commitment,
    pub time: u64,
    pub operation: Operation,
    signature: Signature,
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
        let mut request = Self {
            genesis,
            key: key.public_key(),
            id,
            time,
            operation,
            signature: key.sign(b""),
        };
        request.signature = key.sign(&request.signing_bytes()?);
        Ok(request)
    }
    fn signing_bytes(&self) -> Result<Vec<u8>, CoordinationError> {
        bytes(
            "rocky.coordination-request.v2",
            &(self.genesis, self.key, self.id, self.time, &self.operation),
        )
    }
    pub fn authenticate(&self, genesis: Commitment) -> Result<(), CoordinationError> {
        if self.genesis != genesis || !self.key.verify(&self.signing_bytes()?, &self.signature) {
            return Err(CoordinationError::Denied);
        }
        Ok(())
    }
    pub fn fresh(&self) -> Result<(), CoordinationError> {
        if now()?.abs_diff(self.time) > 30 {
            return Err(CoordinationError::Denied);
        }
        Ok(())
    }
    fn digest(&self) -> Result<Commitment, CoordinationError> {
        Ok(commitment(
            "rocky.coordination-operation.v1",
            &encode(&self.operation)?,
        ))
    }
    /// Administrative mutations keep retry outcomes. Reads keep nothing;
    /// confirmations deduplicate in their session window and retirement is
    /// idempotent.
    fn retained(&self) -> bool {
        !matches!(
            self.operation,
            Operation::Coordinated { .. }
                | Operation::Head { .. }
                | Operation::Lookup { .. }
                | Operation::Confirm(_)
                | Operation::Retire { .. }
                | Operation::Usage
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
            Operation::Head { .. }
                | Operation::Lookup { .. }
                | Operation::Usage
                | Operation::Coordinated {
                    action: crate::coordinated::Action::Head,
                    ..
                }
        )
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Outcome {
    Head(StrongHead),
    CoordinatedHead(crate::coordinated::Head),
    Coordinated(crate::coordinated::Result),
    Receipt(Option<StrongReceipt>),
    Controlled(Commitment),
    Membership(BTreeSet<u64>),
    Replacement(ReplacementRecord),
    /// The signer's retirement floor after `Retire`.
    Floor(u64),
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
            Operation::Coordinated { scope, action } => {
                self.log_authorized(*scope, action, request, time)
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
            Operation::Acquire { scope } | Operation::Takeover { scope, .. } => {
                self.authorized(*scope, request, true, time).map(|_| ())
            }
            Operation::Head { scope } => self.head_authorized(*scope, request, time),
            Operation::Lookup { scope, .. } | Operation::Retire { scope, .. } => {
                self.authorized(*scope, request, false, time).map(|_| ())
            }
            Operation::Replace {
                record,
                certificate,
            } => self.replacement_authorized(request, record, certificate, time),
            Operation::Close { head } => self.closure_authorized(request, head.scope),
            Operation::Activate(activation) => {
                self.closure_authorized(request, activation.head.scope)
            }
            Operation::Confirm(p) => self.authorized(p.scope, request, true, time).map(|_| ()),
            Operation::Register(registration) => {
                let Registration {
                    grant,
                    admission,
                    certificate,
                } = &**registration;
                self.registration_authorized(request, grant, admission.as_ref(), certificate, time)
            }
            Operation::Usage => self.tenant(request.key).map(|_| ()),
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
    /// Applies at the request's own time; deterministic tests only.
    #[cfg(test)]
    pub fn apply(&mut self, request: &Request) -> Result<Outcome, CoordinationError> {
        self.apply_at(request.time, request)
    }
    /// Answers a read-only request from applied state at its authority time.
    pub fn read(&self, request: &Request) -> Result<Outcome, CoordinationError> {
        request.authenticate(self.genesis.id()?)?;
        let time = self.state.time;
        match &request.operation {
            Operation::Coordinated {
                scope,
                action: crate::coordinated::Action::Head,
            } => {
                self.log_authorized(*scope, &crate::coordinated::Action::Head, request, time)?;
                Ok(Outcome::CoordinatedHead(
                    self.state.find(*scope)?.coordinated.head.clone(),
                ))
            }
            Operation::Head { scope } => {
                self.head_authorized(*scope, request, time)?;
                Ok(Outcome::Head(self.state.find(*scope)?.head.clone()))
            }
            Operation::Lookup { scope, request: id } => {
                self.authorized(*scope, request, false, time)?;
                self.state.find(*scope)?;
                self.state.status(*scope, *id).map(Outcome::Receipt)
            }
            Operation::Usage => self.usage(request.key).map(Outcome::Usage),
            _ => Err(CoordinationError::Denied),
        }
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
        let scope = match &request.operation {
            Operation::Register(_) | Operation::Usage => return Some(request.key),
            Operation::Membership { .. } => return None,
            Operation::Control(certificate) => certificate.control.scope,
            Operation::Replace { record, .. } => record.lost_head.scope,
            Operation::Close { head } => head.scope,
            Operation::Activate(activation) => activation.head.scope,
            Operation::Confirm(proposal) => proposal.scope,
            Operation::Coordinated { scope, .. }
            | Operation::Acquire { scope }
            | Operation::Takeover { scope, .. }
            | Operation::Head { scope }
            | Operation::Lookup { scope, .. }
            | Operation::Retire { scope, .. } => *scope,
        };
        let certificate = self.controls.iter().find(|c| c.control.scope == scope)?;
        if certificate.issuer != request.key
            && !certificate.control.executors.contains(&request.key)
            && !certificate
                .control
                .invokers
                .iter()
                .any(|(key, _)| *key == request.key)
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
                used += domain.sessions.bytes() + domain.coordinated.bytes();
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
        let mut keys: BTreeSet<_> = certificate.control.executors.iter().copied().collect();
        for existing in self.controls.iter().filter(|c| {
            c.issuer == certificate.issuer
                && c.control.scope != certificate.control.scope
                && Some(c.control.scope) != retired
                && self.state.find(c.control.scope).is_ok_and(Domain::open)
        }) {
            keys.extend(existing.control.executors.iter().copied());
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
            if old.issuer != certificate.issuer {
                return Err(CoordinationError::Denied);
            }
            if self
                .state
                .find(control.scope)?
                .coordinated
                .head
                .pending
                .is_some()
                || self
                    .state
                    .find(control.scope)?
                    .coordinated
                    .head
                    .members
                    .iter()
                    .any(|m| !control.executors.contains(&m.key))
            {
                return Err(CoordinationError::Backpressure);
            }
            let growth = Self::certificate_bytes(&certificate)?
                .saturating_sub(Self::certificate_bytes(old)?);
            if growth > self.issuer_available(certificate.issuer)? {
                return Err(CoordinationError::Backpressure);
            }
            let domain = self
                .state
                .domains
                .iter_mut()
                .find(|d| d.scope == control.scope)
                .ok_or(CoordinationError::Corrupt)?;
            if !domain.open() {
                return Err(CoordinationError::Denied);
            }
            if control.schema != domain.head.schema || control.handler != domain.head.handler {
                return Err(CoordinationError::Generations);
            }
            domain.head.epoch = domain
                .head
                .epoch
                .checked_add(1)
                .filter(|v| *v <= MAX_COUNTER)
                .ok_or(CoordinationError::StaleEpoch)?;
            domain.head.executor = NodeId([0; 16]);
            domain.owner = control.owner;
            domain.eligible = control.executors.iter().copied().map(node_id).collect();
            domain.invokers = control
                .invokers
                .iter()
                .map(|(key, principal)| (node_id(*key), *principal))
                .collect();
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
        self.state.domains.push(Domain::new(
            control.scope,
            control.owner,
            control.executors.iter().copied().map(node_id).collect(),
            control
                .invokers
                .iter()
                .map(|(key, principal)| (node_id(*key), *principal))
                .collect(),
            control.configuration,
            control.schema,
            control.handler,
        ));
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
            || certificate.control.revision != 1
            || self.genesis_bound(scope)
        {
            return Err(CoordinationError::Denied);
        }
        self.trusted_budget(certificate, None)?;
        certificate.check(&self.genesis.genesis, time)
    }
    fn authorized(
        &self,
        scope: Scope,
        request: &Request,
        executor: bool,
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
        let c = &certificate.control;
        if !(c.executors.contains(&request.key)
            || (!executor && c.invokers.iter().any(|(k, _)| *k == request.key)))
        {
            return Err(CoordinationError::Denied);
        }
        Ok(c)
    }
    /// An owner needs the current head to fence a closure or activation.
    fn head_authorized(
        &self,
        scope: Scope,
        request: &Request,
        time: u64,
    ) -> Result<(), CoordinationError> {
        self.closure_authorized(request, scope)
            .or_else(|_| self.authorized(scope, request, false, time).map(|_| ()))
    }
    fn closure_authorized(&self, request: &Request, scope: Scope) -> Result<(), CoordinationError> {
        if !self.authority(scope, request.key) || self.issuer(scope) != Some(request.key) {
            return Err(CoordinationError::Denied);
        }
        Ok(())
    }
    fn replacement_authorized(
        &self,
        request: &Request,
        record: &ReplacementRecord,
        certificate: &ControlCertificate,
        time: u64,
    ) -> Result<(), CoordinationError> {
        let lost = record.lost_head.scope;
        self.trusted_budget(certificate, Some(lost))?;
        if self.genesis_bound(lost) {
            certificate.verify(&self.genesis.genesis, time)?;
        } else if self.genesis_bound(record.replacement) {
            return Err(CoordinationError::Denied);
        } else {
            certificate.check(&self.genesis.genesis, time)?;
        }
        if certificate.issuer != request.key
            || certificate.control.scope != record.replacement
            || !self.authority(lost, request.key)
            || self
                .controls
                .iter()
                .find(|c| c.control.scope == lost)
                .is_none_or(|c| c.control.owner != certificate.control.owner)
            || certificate.control.schema != record.lost_head.schema
            || certificate.control.handler != record.lost_head.handler
        {
            return Err(CoordinationError::Denied);
        }
        Ok(())
    }
    fn log_authorized(
        &self,
        scope: Scope,
        action: &crate::coordinated::Action,
        request: &Request,
        time: u64,
    ) -> Result<(), CoordinationError> {
        use crate::coordinated::Action;
        match action {
            Action::Configure { .. } => self.closure_authorized(request, scope),
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
            _ => self.authorized(scope, request, true, time).map(|_| ()),
        }?;
        if !matches!(action, Action::Head) && !self.state.find(scope)?.open() {
            return Err(CoordinationError::Denied);
        }
        Ok(())
    }
    pub(crate) fn pending_logs(&self) -> Vec<(Scope, crate::coordinated::Pending)> {
        self.state
            .domains
            .iter()
            .filter(|d| d.open())
            .filter_map(|d| d.coordinated.head.pending.clone().map(|p| (d.scope, p)))
            .collect()
    }
    pub(crate) fn log_head(
        &self,
        scope: Scope,
    ) -> Result<crate::coordinated::Head, CoordinationError> {
        Ok(self.state.find(scope)?.coordinated.head.clone())
    }
    fn coordinated(
        &mut self,
        scope: Scope,
        action: &crate::coordinated::Action,
        request: &Request,
    ) -> Result<Outcome, CoordinationError> {
        use crate::coordinated::Action;
        self.log_authorized(scope, action, request, self.state.time)?;
        if matches!(action, Action::Head) {
            return self.log_head(scope).map(Outcome::CoordinatedHead);
        }
        let domain = self.state.find(scope)?;
        let mut next = domain.coordinated.clone();
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
                    self.authorized(scope, ack, true, ack.time)?;
                    if ack.operation
                        != (Operation::Coordinated {
                            scope,
                            action: Action::Acknowledge {
                                pending: pending.clone(),
                            },
                        })
                        || ack.time > pending.deadline
                        || !domain
                            .coordinated
                            .head
                            .members
                            .iter()
                            .any(|m| m.key == ack.key)
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
                    .map(|d| u64::from(d.coordinated.appends_in(minute)))
                    .sum();
                next.apply(
                    request.key,
                    action,
                    self.state.time,
                    appends < u64::from(crate::coordinated::APPENDS_PER_MINUTE),
                )
            }
            _ => next.apply(request.key, action, self.state.time, true),
        };
        let growth = next.bytes().saturating_sub(domain.coordinated.bytes());
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
        self.state.domain(scope)?.coordinated = next;
        result.map(Outcome::Coordinated)
    }
    fn transition(&mut self, request: &Request) -> Result<Outcome, CoordinationError> {
        let time = self.state.time;
        match &request.operation {
            Operation::Coordinated { scope, action } => self.coordinated(*scope, action, request),
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
            Operation::Replace {
                record,
                certificate,
            } => {
                self.replacement_authorized(request, record, certificate, time)?;
                let known = self
                    .controls
                    .iter()
                    .find(|c| c.control.scope == record.replacement);
                if known.is_some_and(|old| old != certificate) {
                    return Err(CoordinationError::RequestConflict);
                }
                let public = !self.genesis_bound(record.lost_head.scope);
                if known.is_none() {
                    if certificate.control.revision != 1 || (!public && self.controls.len() >= 64) {
                        return Err(CoordinationError::Configuration);
                    }
                    if public {
                        self.public_admits(request.key, certificate)?;
                    }
                }
                let result = self.state.replace(
                    certificate.control.owner,
                    record.clone(),
                    certificate
                        .control
                        .executors
                        .iter()
                        .copied()
                        .map(node_id)
                        .collect(),
                )?;
                if known.is_none() {
                    self.controls.push(certificate.clone());
                    if public {
                        self.count_registration(request.key);
                    }
                }
                let domain = self
                    .state
                    .domains
                    .iter_mut()
                    .find(|d| d.scope == record.replacement)
                    .ok_or(CoordinationError::Corrupt)?;
                domain.invokers = certificate
                    .control
                    .invokers
                    .iter()
                    .map(|(key, principal)| (node_id(*key), *principal))
                    .collect();
                Ok(Outcome::Replacement(result))
            }
            Operation::Activate(activation) => {
                self.closure_authorized(request, activation.head.scope)?;
                let owner = self
                    .controls
                    .iter()
                    .find(|c| c.control.scope == activation.head.scope)
                    .ok_or(CoordinationError::UnknownDomain)?
                    .control
                    .owner;
                self.state.activate(owner, activation).map(Outcome::Head)
            }
            Operation::Close { head } => {
                self.closure_authorized(request, head.scope)?;
                let owner = self
                    .controls
                    .iter()
                    .find(|c| c.control.scope == head.scope)
                    .ok_or(CoordinationError::UnknownDomain)?
                    .control
                    .owner;
                self.state.close(owner, head).map(Outcome::Head)
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
            Operation::Acquire { scope } => {
                let owner = self.authorized(*scope, request, true, time)?.owner;
                // A control update deliberately clears the executor while preserving its fencing epoch.
                let d = self.state.domain(*scope)?;
                if !d.open() {
                    return Err(CoordinationError::Denied);
                }
                if d.head.executor == NodeId([0; 16]) && d.head.epoch > 0 {
                    d.head.executor = node_id(request.key);
                    return Ok(Outcome::Head(d.head.clone()));
                }
                self.state
                    .acquire(*scope, owner, node_id(request.key))
                    .map(Outcome::Head)
            }
            Operation::Takeover {
                scope,
                expected_epoch,
            } => {
                let owner = self.authorized(*scope, request, true, time)?.owner;
                self.state
                    .takeover(*scope, owner, node_id(request.key), *expected_epoch)
                    .map(Outcome::Head)
            }
            Operation::Head { scope } => {
                self.head_authorized(*scope, request, time)?;
                Ok(Outcome::Head(self.state.domain(*scope)?.head.clone()))
            }
            Operation::Lookup { scope, request: id } => {
                self.authorized(*scope, request, false, time)?;
                self.state.lookup(*scope, *id).map(Outcome::Receipt)
            }
            Operation::Retire { scope, through } => {
                self.authorized(*scope, request, false, time)?;
                let available = self.tenant_available(*scope)?;
                self.state
                    .retire(*scope, node_id(request.key), *through, available)
                    .map(Outcome::Floor)
            }
            Operation::Confirm(proposal) => {
                self.authorized(proposal.scope, request, true, time)?;
                if proposal.executor != node_id(request.key) {
                    return Err(CoordinationError::Denied);
                }
                let available = self.tenant_available(proposal.scope)?;
                self.state
                    .confirm(proposal.clone(), available)
                    .map(|r| Outcome::Receipt(Some(r)))
            }
        }
    }
}

#[cfg(test)]
#[path = "machine/public_tests.rs"]
mod public_tests;
#[cfg(test)]
#[path = "machine/replacement_tests.rs"]
mod replacement_tests;
#[cfg(test)]
#[path = "machine/retention_tests.rs"]
mod retention_tests;

#[cfg(test)]
#[path = "machine/coordinated_tests.rs"]
mod coordinated_tests;
