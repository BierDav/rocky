//! Volatile leader acknowledgments and lazy deadline resolution.
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
        tokio::spawn(async move {
            loop {
                let delay = if let Some(service) = weak.upgrade() {
                    let leader = metrics.borrow().current_leader == Some(service.node);
                    if leader && !service.fenced.load(Ordering::SeqCst) {
                        let pending = service.store.pending_logs().await;
                        let time = match now() {
                            Ok(time) => time,
                            Err(_) => return,
                        };
                        let mut next = None;
                        for (log, entry) in pending {
                            if entry.deadline <= time {
                                let _ = service.resolve_log(log, entry).await;
                            } else {
                                next = Some(
                                    next.map_or(entry.deadline, |n: u64| n.min(entry.deadline)),
                                );
                            }
                        }
                        // Failed resolutions retry on the next Raft metrics change.
                        next.map(|deadline| {
                            std::time::Duration::from_secs(deadline.saturating_sub(time))
                        })
                    } else {
                        service.acknowledgments.lock().await.clear();
                        None
                    }
                } else {
                    return;
                };
                if let Some(delay) = delay {
                    tokio::select! {
                        result = metrics.changed() => { if result.is_err() { return; } }
                        () = tokio::time::sleep(delay) => {}
                    }
                } else if metrics.changed().await.is_err() {
                    return;
                }
            }
        });
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
