//! Volatile leader acknowledgments and explicit deadline resolution.
use super::*;

pub(super) struct VolatileAcknowledgments {
    pub term: u64,
    pub pending: crate::coordinated::Pending,
    pub requests: Vec<Request>,
}

impl VoterService {
    pub(super) fn watch_log_deadlines(service: &Arc<Self>) {
        let weak = Arc::downgrade(service);
        let mut metrics = service.raft.metrics();
        let mut deadlines = service.store.deadlines();
        tokio::spawn(async move {
            let mut retry = std::time::Duration::from_millis(100);
            loop {
                let leadership = {
                    let metrics = metrics.borrow_and_update();
                    (metrics.current_leader, metrics.current_term)
                };
                deadlines.borrow_and_update();
                let Some(service) = weak.upgrade() else {
                    return;
                };
                let delay = if leadership.0 == Some(service.node)
                    && !service.fenced.load(Ordering::SeqCst)
                {
                    let Ok(time) = now() else {
                        return;
                    };
                    let mut delay = None;
                    let mut failed = false;
                    for (log, entry) in service.store.pending_logs().await {
                        let wait = if entry.deadline <= time {
                            if service.resolve_log(log, entry).await.is_err() {
                                failed = true;
                                Some(retry)
                            } else {
                                None
                            }
                        } else {
                            Some(std::time::Duration::from_secs(entry.deadline - time))
                        };
                        if let Some(wait) = wait {
                            delay = Some(delay.map_or(wait, |d: std::time::Duration| d.min(wait)));
                        }
                    }
                    retry = if failed {
                        (retry * 2).min(std::time::Duration::from_secs(2))
                    } else {
                        std::time::Duration::from_millis(100)
                    };
                    delay
                } else {
                    service.acknowledgments.lock().await.clear();
                    None
                };
                drop(service);
                tokio::select! {
                    result = metrics.wait_for(|m| (m.current_leader, m.current_term) != leadership) => { if result.is_err() { return; } }
                    result = deadlines.changed() => { if result.is_err() { return; } }
                    () = async { match delay { Some(delay) => tokio::time::sleep(delay).await, None => std::future::pending().await } } => {}
                }
            }
        });
    }
    pub(super) async fn pending_status(
        &self,
        request: &Request,
        log: TipLog,
        position: u64,
        hash: Commitment,
    ) -> Result<LogStatus, CoordinationError> {
        let term = self.log_leader().await?;
        let Outcome::CoordinatedStatus(mut status) = self.store.read(request).await? else {
            return Err(CoordinationError::Corrupt);
        };
        if status.decision == LogDecision::Pending {
            let acks = self.acknowledgments.lock().await;
            if let Some(acks) = acks.get(&log).filter(|a| {
                a.term == term && a.pending.position == position && a.pending.hash == hash
            }) {
                for member in &mut status.members {
                    if acks
                        .requests
                        .iter()
                        .any(|r| r.key.as_bytes() == &member.key)
                    {
                        member.acknowledged = position;
                    }
                }
            }
        }
        // Do not return promises belonging to a term that just ended.
        let metrics = self.raft.metrics();
        let current = metrics.borrow();
        if current.current_term != term || current.current_leader != Some(self.node) {
            return Err(CoordinationError::Unavailable);
        }
        Ok(status)
    }
    async fn log_leader(&self) -> Result<u64, CoordinationError> {
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            self.raft.ensure_linearizable(),
        )
        .await
        .map_err(|_| CoordinationError::Unavailable)?
        .map_err(|_| CoordinationError::Unavailable)?;
        let metrics = self.raft.metrics();
        let metrics = metrics.borrow();
        if metrics.current_leader != Some(self.node) || self.fenced.load(Ordering::SeqCst) {
            return Err(CoordinationError::Unavailable);
        }
        Ok(metrics.current_term)
    }
    async fn resolve_log(
        &self,
        log: TipLog,
        pending: crate::coordinated::Pending,
    ) -> Result<Outcome, CoordinationError> {
        let span = tracing::debug_span!(
            "resolve",
            position = pending.position,
            outcome = tracing::field::Empty
        );
        let result =
            tracing::Instrument::instrument(self.resolve_log_inner(log, pending), span.clone())
                .await;
        span.record("outcome", outcome(&result));
        result
    }
    async fn resolve_log_inner(
        &self,
        log: TipLog,
        pending: crate::coordinated::Pending,
    ) -> Result<Outcome, CoordinationError> {
        let term = self.log_leader().await?;
        let acknowledgments = self
            .acknowledgments
            .lock()
            .await
            .get(&log)
            .filter(|a| a.term == term && a.pending == pending)
            .map_or_else(Vec::new, |a| a.requests.clone());
        let head = self.store.log_head(log).await?;
        if head.pending.as_ref() != Some(&pending) {
            return Err(CoordinationError::StaleHead);
        }
        if now()? < pending.deadline
            && !head
                .members
                .iter()
                .all(|m| acknowledgments.iter().any(|r| r.key == m.key))
        {
            return Err(CoordinationError::Backpressure);
        }
        let request = Request::sign(
            self.genesis.id()?,
            nonce(),
            Operation::Coordinated {
                log,
                action: crate::coordinated::Action::Resolve {
                    pending: pending.clone(),
                    acknowledgments,
                },
            },
            &self.key,
        )?;
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            self.raft.client_write(Proposal {
                time: now()?,
                request,
            }),
        )
        .await
        .map_err(|_| CoordinationError::Unavailable)?
        .map_err(|_| CoordinationError::Unavailable)?
        .data;
        if result.is_ok() {
            let mut acks = self.acknowledgments.lock().await;
            if acks.get(&log).is_some_and(|a| a.pending == pending) {
                acks.remove(&log);
            }
        }
        result
    }
    pub(super) async fn acknowledge_log(
        &self,
        request: &Request,
        log: TipLog,
        pending: &crate::coordinated::Pending,
    ) -> Result<Outcome, CoordinationError> {
        let span = tracing::debug_span!(
            "acknowledge",
            position = pending.position,
            outcome = tracing::field::Empty
        );
        let result = tracing::Instrument::instrument(
            self.acknowledge_log_inner(request, log, pending),
            span.clone(),
        )
        .await;
        span.record("outcome", outcome(&result));
        result
    }
    async fn acknowledge_log_inner(
        &self,
        request: &Request,
        log: TipLog,
        pending: &crate::coordinated::Pending,
    ) -> Result<Outcome, CoordinationError> {
        let term = self.log_leader().await?;
        let head = self.store.log_head(log).await?;
        if head.pending.as_ref() != Some(pending) {
            return Err(CoordinationError::StaleHead);
        }
        if !head.members.iter().any(|m| m.key == request.key) {
            return Err(CoordinationError::Denied);
        }
        if now()? >= pending.deadline || request.time > pending.deadline {
            return Err(CoordinationError::Expired);
        }
        {
            let mut acks = self.acknowledgments.lock().await;
            let entry = acks.entry(log).or_insert_with(|| VolatileAcknowledgments {
                term,
                pending: pending.clone(),
                requests: Vec::new(),
            });
            if entry.term != term || entry.pending != *pending {
                *entry = VolatileAcknowledgments {
                    term,
                    pending: pending.clone(),
                    requests: Vec::new(),
                };
            }
            if !entry.requests.iter().any(|r| r.key == request.key) {
                entry.requests.push(request.clone());
            }
        }
        match self.resolve_log(log, pending.clone()).await {
            Err(CoordinationError::Backpressure) => Ok(Outcome::Coordinated(
                crate::coordinated::Result::Acknowledged,
            )),
            result => result,
        }
    }
}

fn outcome(result: &Result<Outcome, CoordinationError>) -> &'static str {
    match result {
        Ok(_) => "ok",
        Err(error) => error.category(),
    }
}
