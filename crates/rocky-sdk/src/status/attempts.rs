use super::*;
use crate::{Failure, service::ServiceError};
use std::sync::{
    Arc, Mutex, PoisonError,
    atomic::{AtomicU64, Ordering},
};
use tokio::{sync::watch, time::Instant};

pub(crate) fn epoch_millis() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
}
#[derive(Default)]
struct Inner {
    records: Mutex<Vec<AttemptObservation>>,
    partial: Mutex<std::collections::BTreeMap<String, crate::sync::Transfer>>,
    next: AtomicU64,
    revision: watch::Sender<u64>,
}
#[derive(Clone, Default)]
pub(crate) struct Observations(Arc<Inner>);
impl Observations {
    pub fn touch(&self) {
        self.0
            .revision
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }
    pub fn partial_transfer(&self, target: &str) -> Option<crate::sync::Transfer> {
        self.0
            .partial
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(target)
    }
    pub fn record_partial(&self, target: String, transfer: crate::sync::Transfer) {
        let mut partial = self
            .0
            .partial
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        partial
            .entry(target)
            .and_modify(|prior| prior.extend(transfer.clone()))
            .or_insert(transfer);
        while partial.len() > 512 {
            partial.pop_first();
        }
    }
    pub fn next_id(&self) -> u64 {
        self.0.next.load(Ordering::Relaxed)
    }
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.0.revision.subscribe()
    }
    pub fn snapshot(&self) -> Vec<AttemptObservation> {
        self.0
            .records
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
    fn store(&self, value: AttemptObservation) {
        let mut records = self
            .0
            .records
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        records.retain(|record| record.id != value.id);
        records.push(value);
        if records.len() > 512
            && let Some(oldest) = records
                .iter()
                .filter(|record| record.outcome.is_some())
                .min_by_key(|record| record.id)
                .map(|record| record.id)
        {
            records.retain(|record| record.id != oldest);
        }
        drop(records);
        self.0
            .revision
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }
    pub fn begin(
        &self,
        target: String,
        operation: AttemptOperation,
        stage: AttemptStage,
    ) -> Attempt {
        let value = AttemptObservation {
            id: self.0.next.fetch_add(1, Ordering::Relaxed),
            target,
            operation,
            stage,
            started_at_epoch_millis: epoch_millis(),
            ended_at_epoch_millis: None,
            elapsed_millis: None,
            outcome: None,
            failure: None,
        };
        self.store(value.clone());
        Attempt {
            observations: self.clone(),
            value,
            started: Instant::now(),
        }
    }
}
pub(crate) struct Attempt {
    observations: Observations,
    value: AttemptObservation,
    started: Instant,
}
impl Attempt {
    pub fn current_stage(&self) -> AttemptStage {
        self.value.stage.clone()
    }
    pub fn stage(&mut self, stage: AttemptStage) {
        self.value.stage = stage;
        self.observations.store(self.value.clone());
    }
    pub fn finish_empty(mut self) -> AttemptObservation {
        self.end(AttemptOutcome::NoResult);
        self.value.clone()
    }
    pub fn finish(mut self, failure: Option<AttemptFailure>) -> AttemptObservation {
        self.value.failure = failure;
        self.end(if self.value.failure.is_some() {
            AttemptOutcome::Failed
        } else {
            AttemptOutcome::Completed
        });
        self.value.clone()
    }
    fn end(&mut self, outcome: AttemptOutcome) {
        self.value.ended_at_epoch_millis = epoch_millis();
        self.value.elapsed_millis =
            Some(self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64);
        self.value.outcome = Some(outcome);
        self.observations.store(self.value.clone());
    }
}
impl Drop for Attempt {
    fn drop(&mut self) {
        if self.value.outcome.is_none() {
            self.end(AttemptOutcome::Cancelled);
        }
    }
}
pub(crate) fn failure(error: &Failure, stage: &AttemptStage) -> AttemptFailure {
    let mut kind = FailureKind::Other;
    let mut code = None;
    if error.is::<tokio::time::error::Elapsed>() {
        kind = FailureKind::Timeout;
    }
    if let Some(error) = error.downcast_ref::<std::io::Error>() {
        kind = if error.kind() == std::io::ErrorKind::TimedOut {
            FailureKind::Timeout
        } else {
            FailureKind::Transport
        };
    }
    if let Some(service) = error.downcast_ref::<ServiceError>() {
        code = Some(service.code().into());
        kind = match service {
            ServiceError::Unreachable(_) => FailureKind::Transport,
            ServiceError::TimedOut(_) => FailureKind::Timeout,
            ServiceError::CredentialsExpired => FailureKind::LocalAuthorization,
            _ => FailureKind::Other,
        };
    }
    if let Some(secure) = error.downcast_ref::<rocky_secure::Error>() {
        kind = match secure {
            rocky_secure::Error::Io(error) => {
                if error.kind() == std::io::ErrorKind::TimedOut {
                    FailureKind::Timeout
                } else {
                    FailureKind::Transport
                }
            }
            rocky_secure::Error::Protocol(_) | rocky_secure::Error::Incompatible(_) => {
                FailureKind::Protocol
            }
            rocky_secure::Error::Coordination(_) => FailureKind::Coordination,
            rocky_secure::Error::Application(cause)
                if cause.is::<rocky_secure::network::link::Busy>() =>
            {
                code = Some("busy".into());
                FailureKind::RemoteRefusal
            }
            rocky_secure::Error::Application(cause)
                if cause.is::<rocky_secure::network::Revoked>() =>
            {
                code = Some("revoked".into());
                FailureKind::LocalAuthorization
            }
            _ => FailureKind::Other,
        };
    }
    if error.is::<crate::enrollment::Revoked>() {
        kind = FailureKind::LocalAuthorization;
        code = Some("revoked".into());
    }
    // A transport that erases its error type still failed during a dial, not admission.
    if kind == FailureKind::Other && *stage == AttemptStage::Dial {
        kind = FailureKind::Transport;
    }
    AttemptFailure {
        kind,
        code,
        detail: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_is_an_observation_and_diagnostic_text_does_not_establish_revocation() {
        let observations = Observations::default();
        let attempt = observations.begin(
            "peer".into(),
            AttemptOperation::OpenLink,
            AttemptStage::Admission,
        );
        assert_eq!(observations.snapshot()[0].outcome, None);
        drop(attempt);
        assert_eq!(
            observations.snapshot()[0].outcome,
            Some(AttemptOutcome::Cancelled)
        );
        let claim: Failure = Box::new(crate::node::Refused("this device is revoked".into()));
        let failure = failure(&claim, &AttemptStage::Admission);
        assert_eq!(failure.kind, FailureKind::Other);
        assert_eq!(failure.code, None);
        let busy: Failure = Box::new(rocky_secure::Error::Application(Box::new(
            rocky_secure::network::link::Busy,
        )));
        assert_eq!(
            super::failure(&busy, &AttemptStage::Admission).kind,
            FailureKind::RemoteRefusal
        );
    }

    #[test]
    fn completed_diagnostics_are_bounded_without_displacing_an_active_attempt() {
        let observations = Observations::default();
        let active = observations.begin(
            "active".into(),
            AttemptOperation::Reconcile,
            AttemptStage::Reconciliation,
        );
        for _ in 0..600 {
            observations
                .begin(
                    "peer".into(),
                    AttemptOperation::OpenLink,
                    AttemptStage::Dial,
                )
                .finish(None);
        }
        assert_eq!(observations.snapshot().len(), 512);
        assert!(
            observations
                .snapshot()
                .iter()
                .any(|attempt| attempt.target == "active" && attempt.outcome.is_none())
        );
        active.finish(None);
        assert_eq!(observations.snapshot().len(), 512);
    }
}
