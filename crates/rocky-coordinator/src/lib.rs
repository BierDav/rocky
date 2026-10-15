//! Trusted metadata coordination: a certified persistent Raft service and
//! the coordinated log. Application payloads and policy remain outside it.
pub use retention::Limits;
use rocky_core::*;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Domain {
    scope: Scope,
    coordinated: coordinated::Logs,
    /// Closed by its owner: the identity never reopens.
    closed: bool,
}
impl Domain {
    fn new(scope: Scope, configuration: Configuration) -> Self {
        Self {
            scope,
            coordinated: coordinated::Logs::new(scope, configuration),
            closed: false,
        }
    }
    fn open(&self) -> bool {
        !self.closed
    }
}
/// Retained state of one coordinator, by count and accounted bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Retention {
    pub domains: usize,
    pub bytes: usize,
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
        self.domains.iter().map(|d| d.coordinated.bytes()).sum()
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
