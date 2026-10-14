//! Trusted metadata coordination: memory fixtures and a certified persistent Raft service.
//! Application payloads and policy remain outside coordinator execution.
pub use retention::{Limits, Status};
use rocky_core::*;
use std::sync::Mutex;

/// Explicit acknowledged loss; the closed domain retains its head and bounded request windows.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReplacementRecord {
    pub lost_head: StrongHead,
    pub replacement: Scope,
    pub acknowledged_loss: bool,
    pub uncertain_effects: bool,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Domain {
    scope: Scope,
    coordinated: coordinated::Log,
    owner: PrincipalId,
    eligible: Vec<NodeId>,
    invokers: Vec<(NodeId, PrincipalId)>,
    head: StrongHead,
    sessions: retention::Sessions,
    replacement: Option<ReplacementRecord>,
    /// Closed by its owner: the identity never executes or reopens.
    closed: bool,
    /// The last generation activation, for an idempotent retry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    activation: Option<Activation>,
}
impl Domain {
    fn new(
        scope: Scope,
        owner: PrincipalId,
        eligible: Vec<NodeId>,
        invokers: Vec<(NodeId, PrincipalId)>,
        configuration: Configuration,
        schema: u32,
        handler: u32,
    ) -> Self {
        Self {
            scope,
            coordinated: coordinated::Log::new(scope, configuration),
            owner,
            eligible,
            invokers,
            head: StrongHead {
                scope,
                configuration,
                executor: NodeId([0; 16]),
                epoch: 0,
                schema,
                handler,
                sequence: 0,
                head: genesis(scope, configuration),
            },
            sessions: Default::default(),
            replacement: None,
            closed: false,
            activation: None,
        }
    }
    fn open(&self) -> bool {
        self.replacement.is_none() && !self.closed
    }
}
/// Retained state of one coordinator, by count and accounted bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Retention {
    pub domains: usize,
    pub sessions: usize,
    pub entries: usize,
    pub bytes: usize,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct State {
    domains: Vec<Domain>,
    available: bool,
    drop_reply: bool,
    limits: Limits,
    /// Authority time in seconds; it never moves backwards.
    time: u64,
}
pub struct MemoryCoordinator {
    state: Mutex<State>,
    /// A controllable clock; the system clock when unset.
    clock: Mutex<Option<u64>>,
}
impl Default for MemoryCoordinator {
    fn default() -> Self {
        Self::with_limits(Limits::default())
    }
}
impl State {
    fn new(limits: Limits) -> Self {
        Self {
            domains: Vec::new(),
            available: true,
            drop_reply: false,
            limits,
            time: 0,
        }
    }
    /// Advances authority time, clamping a backward clock, and retires what it passed.
    fn advance(&mut self, time: u64) {
        if time > self.time {
            self.time = time;
            let limits = self.limits;
            for domain in &mut self.domains {
                domain.sessions.prune(&limits, time);
            }
        }
    }
    fn retention(&self) -> Retention {
        Retention {
            domains: self.domains.len(),
            sessions: self.domains.iter().map(|d| d.sessions.sessions()).sum(),
            entries: self.domains.iter().map(|d| d.sessions.entries()).sum(),
            bytes: self.bytes(),
        }
    }
    fn bytes(&self) -> usize {
        self.domains
            .iter()
            .map(|d| d.sessions.bytes() + d.coordinated.bytes())
            .sum()
    }
    fn domain(&mut self, scope: Scope) -> Result<&mut Domain, CoordinationError> {
        if !self.available {
            return Err(CoordinationError::Unavailable);
        }
        self.domains
            .iter_mut()
            .find(|d| d.scope == scope)
            .ok_or(CoordinationError::UnknownDomain)
    }
    fn replace(
        &mut self,
        control: PrincipalId,
        record: ReplacementRecord,
        eligible: Vec<NodeId>,
    ) -> Result<ReplacementRecord, CoordinationError> {
        let old = self.domain(record.lost_head.scope)?;
        if old.owner != control
            || old.coordinated.head.pending.is_some()
            || old.closed
            || !record.acknowledged_loss
            || !record.uncertain_effects
            || record.replacement == record.lost_head.scope
            || record.replacement.mesh != record.lost_head.scope.mesh
            || record.replacement.app != record.lost_head.scope.app
            || eligible.is_empty()
        {
            return Err(CoordinationError::Denied);
        }
        if let Some(retained) = &old.replacement {
            return if *retained == record {
                Ok(retained.clone())
            } else {
                Err(CoordinationError::RequestConflict)
            };
        }
        if old.head != record.lost_head {
            return Err(CoordinationError::StaleHead);
        }
        if self
            .domains
            .iter()
            .any(|domain| domain.scope == record.replacement)
        {
            return Err(CoordinationError::Configuration);
        }
        let next_epoch = record
            .lost_head
            .epoch
            .checked_add(1)
            .filter(|v| *v <= MAX_COUNTER)
            .ok_or(CoordinationError::StaleEpoch)?;
        let lost = &record.lost_head;
        self.domains.push(Domain::new(
            record.replacement,
            control,
            eligible.clone(),
            eligible.iter().map(|node| (*node, control)).collect(),
            lost.configuration,
            lost.schema,
            lost.handler,
        ));
        let old = self.domain(record.lost_head.scope)?;
        old.head.epoch = next_epoch;
        old.head.executor = NodeId([0; 16]);
        old.replacement = Some(record.clone());
        Ok(record)
    }
    /// Owner-initiated closure at an exact head. The closed identity keeps
    /// its final head and never executes or reopens.
    fn close(
        &mut self,
        control: PrincipalId,
        head: &StrongHead,
    ) -> Result<StrongHead, CoordinationError> {
        let domain = self.domain(head.scope)?;
        if domain.owner != control
            || domain.replacement.is_some()
            || domain.coordinated.head.pending.is_some()
        {
            return Err(CoordinationError::Denied);
        }
        if domain.closed {
            return Ok(domain.head.clone());
        }
        if domain.head != *head {
            return Err(CoordinationError::StaleHead);
        }
        domain.head.epoch = head
            .epoch
            .checked_add(1)
            .filter(|v| *v <= MAX_COUNTER)
            .ok_or(CoordinationError::StaleEpoch)?;
        domain.head.executor = NodeId([0; 16]);
        domain.closed = true;
        Ok(domain.head.clone())
    }
    /// Moves an open domain to the next definition generation at exactly
    /// `activation.head`, by its owner. Executor and epoch stay; only
    /// confirmations of the new generations are accepted afterwards.
    pub(crate) fn activate(
        &mut self,
        control: PrincipalId,
        activation: &Activation,
    ) -> Result<StrongHead, CoordinationError> {
        let domain = self.domain(activation.head.scope)?;
        if domain.owner != control || !domain.open() || !activation.valid() {
            return Err(CoordinationError::Denied);
        }
        if domain.activation.as_ref() == Some(activation) {
            return Ok(domain.head.clone());
        }
        if domain.head != activation.head {
            return Err(CoordinationError::StaleHead);
        }
        domain.head.schema = activation.schema;
        domain.head.handler = activation.handler;
        domain.activation = Some(activation.clone());
        Ok(domain.head.clone())
    }
    fn acquire(
        &mut self,
        scope: Scope,
        control: PrincipalId,
        executor: NodeId,
    ) -> Result<StrongHead, CoordinationError> {
        let d = self.domain(scope)?;
        authorize(d, control, executor)?;
        if d.head.epoch == 0 {
            d.head.epoch = 1;
            d.head.executor = executor;
        } else if d.head.executor != executor {
            return Err(CoordinationError::StaleEpoch);
        }
        Ok(d.head.clone())
    }
    fn takeover(
        &mut self,
        scope: Scope,
        control: PrincipalId,
        executor: NodeId,
        expected_epoch: u64,
    ) -> Result<StrongHead, CoordinationError> {
        let d = self.domain(scope)?;
        authorize(d, control, executor)?;
        if d.head.epoch != expected_epoch || expected_epoch == 0 {
            return Err(CoordinationError::StaleEpoch);
        }
        d.head.epoch = d
            .head
            .epoch
            .checked_add(1)
            .filter(|v| *v <= MAX_COUNTER)
            .ok_or(CoordinationError::StaleEpoch)?;
        d.head.executor = executor;
        Ok(d.head.clone())
    }
    fn find(&self, scope: Scope) -> Result<&Domain, CoordinationError> {
        if !self.available {
            return Err(CoordinationError::Unavailable);
        }
        self.domains
            .iter()
            .find(|d| d.scope == scope)
            .ok_or(CoordinationError::UnknownDomain)
    }
    /// `None` while admission is open: a confirmation may still follow.
    fn lookup(
        &mut self,
        scope: Scope,
        id: RequestId,
    ) -> Result<Option<StrongReceipt>, CoordinationError> {
        self.status(scope, id)
    }
    fn status(
        &self,
        scope: Scope,
        id: RequestId,
    ) -> Result<Option<StrongReceipt>, CoordinationError> {
        match self
            .find(scope)?
            .sessions
            .status(&self.limits, self.time, id)
        {
            Status::Confirmed(receipt) => Ok(Some(*receipt)),
            Status::Open => Ok(None),
            Status::NotConfirmed => Err(CoordinationError::Expired),
            Status::Retired => Err(CoordinationError::Retired),
        }
    }
    fn retire(
        &mut self,
        scope: Scope,
        node: NodeId,
        through: u64,
        available: usize,
    ) -> Result<u64, CoordinationError> {
        let (limits, now) = (self.limits, self.time);
        let available = available.min(limits.total_bytes.saturating_sub(self.bytes()));
        if self
            .domain(scope)?
            .sessions
            .retirement_cost(&limits, now, node, through)
            > available
        {
            return Err(CoordinationError::Backpressure);
        }
        self.domain(scope)?
            .sessions
            .retire(&limits, now, node, through)
    }
    /// Confirms `p`, adding at most `available` accounted bytes.
    fn confirm(
        &mut self,
        p: Confirmation,
        available: usize,
    ) -> Result<StrongReceipt, CoordinationError> {
        let (limits, now) = (self.limits, self.time);
        let available = available.min(limits.total_bytes.saturating_sub(self.bytes()));
        let d = self.domain(p.scope)?;
        if !d.eligible.contains(&p.executor) || !d.invokers.contains(&(p.request.node, p.caller)) {
            return Err(CoordinationError::Denied);
        }
        if p.configuration != d.head.configuration {
            return Err(CoordinationError::Configuration);
        }
        // Historical identity checks precede current fencing/generation checks.
        match d.sessions.status(&limits, now, p.request) {
            Status::Confirmed(old) => {
                if old.confirmation.request_hash != p.request_hash {
                    return Err(CoordinationError::RequestConflict);
                }
                if old.confirmation != p {
                    return Err(CoordinationError::AlreadyConfirmed(old));
                }
                return Ok(*old);
            }
            Status::Open => {}
            Status::NotConfirmed => return Err(CoordinationError::Expired),
            Status::Retired => return Err(CoordinationError::Retired),
        }
        if !d.open() {
            return Err(CoordinationError::Denied);
        }
        if p.schema != d.head.schema || p.handler != d.head.handler {
            return Err(CoordinationError::Generations);
        }
        if p.executor != d.head.executor || p.epoch != d.head.epoch || p.epoch == 0 {
            return Err(CoordinationError::StaleEpoch);
        }
        if p.expected_sequence != d.head.sequence || p.expected_head != d.head.head {
            return Err(CoordinationError::StaleHead);
        }
        let sequence = p
            .expected_sequence
            .checked_add(1)
            .filter(|v| *v <= MAX_COUNTER)
            .ok_or(CoordinationError::StaleHead)?;
        if p.new_head != successor(p.expected_head, sequence, p.request_hash, p.result) {
            return Err(CoordinationError::InvalidCommitment);
        }
        let receipt = StrongReceipt {
            confirmation: p,
            sequence,
        };
        d.sessions.admit(&limits, now, &receipt, available)?;
        d.sessions.insert(receipt.clone())?;
        d.head.sequence = sequence;
        d.head.head = receipt.confirmation.new_head;
        if self.drop_reply {
            self.drop_reply = false;
            return Err(CoordinationError::Unavailable);
        }
        Ok(receipt)
    }
}
fn authorize(d: &Domain, control: PrincipalId, executor: NodeId) -> Result<(), CoordinationError> {
    if !d.open() || control != d.owner || !d.eligible.contains(&executor) {
        Err(CoordinationError::Denied)
    } else {
        Ok(())
    }
}
impl MemoryCoordinator {
    pub fn with_limits(limits: Limits) -> Self {
        Self {
            state: Mutex::new(State::new(limits)),
            clock: Mutex::new(None),
        }
    }
    /// Locks the state at the current authority time.
    fn state(&self) -> Result<std::sync::MutexGuard<'_, State>, CoordinationError> {
        let time = match *self.clock.lock().map_err(|_| CoordinationError::Corrupt)? {
            Some(time) => time,
            None => certified::now()?,
        };
        let mut state = self.state.lock().map_err(|_| CoordinationError::Corrupt)?;
        state.advance(time);
        Ok(state)
    }
    /// Sets the controllable clock. Authority time never moves backwards.
    pub fn set_time(&self, seconds: u64) -> Result<(), CoordinationError> {
        *self.clock.lock().map_err(|_| CoordinationError::Corrupt)? = Some(seconds);
        Ok(())
    }
    pub fn time(&self) -> Result<u64, CoordinationError> {
        Ok(self.state()?.time)
    }
    pub fn retention(&self) -> Result<Retention, CoordinationError> {
        Ok(self.state()?.retention())
    }
    pub fn register(
        &self,
        scope: Scope,
        owner: PrincipalId,
        configuration: Configuration,
        eligible: Vec<NodeId>,
        schema: u32,
        handler: u32,
    ) -> Result<(), CoordinationError> {
        let mut s = self.state()?;
        if s.domains.iter().any(|d| d.scope == scope) {
            return Err(CoordinationError::Denied);
        }
        let invokers = eligible.iter().map(|node| (*node, owner)).collect();
        s.domains.push(Domain::new(
            scope,
            owner,
            eligible,
            invokers,
            configuration,
            schema,
            handler,
        ));
        Ok(())
    }
    /// Explicit development invocation grants, independent of executor eligibility.
    pub fn set_invokers(
        &self,
        scope: Scope,
        control: PrincipalId,
        invokers: Vec<(NodeId, PrincipalId)>,
    ) -> Result<(), CoordinationError> {
        let mut state = self.state()?;
        let domain = state.domain(scope)?;
        if domain.owner != control {
            return Err(CoordinationError::Denied);
        }
        domain.invokers = invokers;
        Ok(())
    }
    /// Closes the old execution role without rewinding its confirmed audit identity.
    pub fn replace_domain(
        &self,
        control: PrincipalId,
        record: ReplacementRecord,
        eligible: Vec<NodeId>,
    ) -> Result<ReplacementRecord, CoordinationError> {
        self.state()?.replace(control, record, eligible)
    }
    pub fn replacement(
        &self,
        scope: Scope,
    ) -> Result<Option<ReplacementRecord>, CoordinationError> {
        Ok(self.state()?.domain(scope)?.replacement.clone())
    }
    /// Owner-initiated closure at `head`; see [`State::close`].
    pub fn close_domain(
        &self,
        control: PrincipalId,
        head: &StrongHead,
    ) -> Result<StrongHead, CoordinationError> {
        self.state()?.close(control, head)
    }
    pub fn set_available(&self, available: bool) -> Result<(), CoordinationError> {
        self.state()?.available = available;
        Ok(())
    }
    /// Accept the next valid proposal, then simulate a lost reply.
    pub fn drop_next_reply(&self) -> Result<(), CoordinationError> {
        self.state()?.drop_reply = true;
        Ok(())
    }
    pub fn acquire(
        &self,
        scope: Scope,
        control: PrincipalId,
        executor: NodeId,
    ) -> Result<StrongHead, CoordinationError> {
        self.state()?.acquire(scope, control, executor)
    }
    pub fn takeover(
        &self,
        scope: Scope,
        control: PrincipalId,
        executor: NodeId,
        expected_epoch: u64,
    ) -> Result<StrongHead, CoordinationError> {
        self.state()?
            .takeover(scope, control, executor, expected_epoch)
    }
    pub fn head(&self, scope: Scope) -> Result<StrongHead, CoordinationError> {
        Ok(self.state()?.domain(scope)?.head.clone())
    }
    /// The owner's activation of the next definition generation; see
    /// [`Activation`]. Repeating the same activation returns the head.
    pub fn activate(
        &self,
        control: PrincipalId,
        activation: &Activation,
    ) -> Result<StrongHead, CoordinationError> {
        self.state()?.activate(control, activation)
    }
    /// The identity's status; see [`Status`].
    pub fn status(&self, scope: Scope, id: RequestId) -> Result<Status, CoordinationError> {
        let mut state = self.state()?;
        let (limits, now) = (state.limits, state.time);
        Ok(state.domain(scope)?.sessions.status(&limits, now, id))
    }
    pub fn lookup(
        &self,
        scope: Scope,
        id: RequestId,
    ) -> Result<Option<StrongReceipt>, CoordinationError> {
        self.state()?.lookup(scope, id)
    }
    pub fn verify(&self, receipt: &StrongReceipt) -> Result<(), CoordinationError> {
        match self.lookup(receipt.confirmation.scope, receipt.confirmation.request) {
            Ok(Some(retained)) if retained == *receipt => Ok(()),
            Ok(_) | Err(CoordinationError::Expired) => Err(CoordinationError::InvalidCommitment),
            Err(error) => Err(error),
        }
    }
    pub fn confirm(&self, p: Confirmation) -> Result<StrongReceipt, CoordinationError> {
        self.state()?.confirm(p, usize::MAX)
    }
    /// Retires `node`'s identities through `through`; returns its floor.
    pub fn retire(
        &self,
        scope: Scope,
        node: NodeId,
        through: u64,
    ) -> Result<u64, CoordinationError> {
        self.state()?.retire(scope, node, through, usize::MAX)
    }
}

impl Coordinator for MemoryCoordinator {
    fn head(&self, scope: Scope) -> CoordinationFuture<'_, StrongHead> {
        Box::pin(async move { self.head(scope) })
    }
    fn lookup(&self, scope: Scope, id: RequestId) -> CoordinationFuture<'_, Option<StrongReceipt>> {
        Box::pin(async move { self.lookup(scope, id) })
    }
    fn confirm(&self, proposal: Confirmation) -> CoordinationFuture<'_, StrongReceipt> {
        Box::pin(async move { self.confirm(proposal) })
    }
    fn verify<'a>(&'a self, receipt: &'a StrongReceipt) -> CoordinationFuture<'a, ()> {
        Box::pin(async move { self.verify(receipt) })
    }
    fn retire(&self, scope: Scope, node: NodeId, through: u64) -> CoordinationFuture<'_, ()> {
        Box::pin(async move { self.retire(scope, node, through).map(|_| ()) })
    }
}

/// Certified bootstrap and authenticated durable service.
pub mod certified;
mod client;
mod machine;
mod raft_store;
mod retention;
pub mod service;
mod transport;

/// Ordered metadata for coordinated ordinary writes.
pub mod coordinated;
