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
            "rocky.coordination-request.v4",
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
    /// Administrative mutations keep retry outcomes. Reads keep nothing, and
    /// log actions deduplicate by their own sequences.
    fn retained(&self) -> bool {
        !matches!(
            self.operation,
            Operation::Coordinated { .. } | Operation::Usage
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
                | Operation::Coordinated {
                    action: crate::coordinated::Action::Head,
                    ..
                }
        )
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Outcome {
    CoordinatedHead(Box<crate::coordinated::Head>),
    Coordinated(crate::coordinated::Result),
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
                scope,
                action: crate::coordinated::Action::Head,
            } => {
                self.log_authorized(*scope, &crate::coordinated::Action::Head, request, time)?;
                Ok(Outcome::CoordinatedHead(Box::new(
                    self.state.find(*scope)?.coordinated.head.clone(),
                )))
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
            Operation::Close { scope } | Operation::Coordinated { scope, .. } => *scope,
        };
        let certificate = self.controls.iter().find(|c| c.control.scope == scope)?;
        if certificate.issuer != request.key && !certificate.control.trusted.contains(&request.key)
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
                used += domain.coordinated.bytes();
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
                    .any(|m| !control.trusted.contains(&m.key))
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
            || certificate.control.revision != 1
            || self.genesis_bound(scope)
        {
            return Err(CoordinationError::Denied);
        }
        self.trusted_budget(certificate, None)?;
        certificate.check(&self.genesis.genesis, time)
    }
    /// A trusted node of `scope` under its current control.
    fn authorized(
        &self,
        scope: Scope,
        request: &Request,
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
        if !c.trusted.contains(&request.key) {
            return Err(CoordinationError::Denied);
        }
        Ok(c)
    }
    fn closure_authorized(&self, request: &Request, scope: Scope) -> Result<(), CoordinationError> {
        if !self.authority(scope, request.key) || self.issuer(scope) != Some(request.key) {
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
            // A trusted node or the authority submits; the authority signs.
            Action::Cut { record, .. } => {
                let signer = rocky_credentials::SignedCut::from_bytes(record)
                    .map_err(|_| CoordinationError::Denied)?
                    .signer;
                if !self.authority(scope, signer) || self.issuer(scope) != Some(signer) {
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
            return self
                .log_head(scope)
                .map(Box::new)
                .map(Outcome::CoordinatedHead);
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
                    self.authorized(scope, ack, ack.time)?;
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
            Action::Cut { record, authority } => next.cut(scope, record, authority),
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
#[path = "machine/coordinated_tests.rs"]
mod coordinated_tests;
#[cfg(test)]
#[path = "machine/cut_tests.rs"]
mod cut_tests;
