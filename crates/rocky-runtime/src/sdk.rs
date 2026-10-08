//! JSON v1 transport contract. See `docs/runtime/sdk-contract.md`.
use crate::*;
use rocky_core::Coordinator;
use serde_json::{Value, json};
use std::collections::BTreeMap;

mod contract;
pub use contract::{BoundValue, Call, OperationRef, Query, Reply, Request, SdkError, VERSION};

struct Subscription<R> {
    observer: tokio::sync::Mutex<ReadObserver<R>>,
    cancelled: watch::Sender<bool>,
}
struct Subscriptions<R> {
    closed: bool,
    next: u64,
    entries: BTreeMap<u64, Arc<Subscription<R>>>,
}
/// The host supplies an already authorized domain; this session cannot open files
/// or install applications. Dropping it cancels subscriptions, not admitted writes.
pub struct Session<A: StoredApplication> {
    replica: Replica<A>,
    subscriptions: Mutex<Subscriptions<A::Row>>,
}
impl<A: StoredApplication> Drop for Session<A> {
    fn drop(&mut self) {
        if let Ok(subscriptions) = self.subscriptions.lock() {
            for subscription in subscriptions.entries.values() {
                subscription.cancelled.send_replace(true);
            }
        }
    }
}
impl<A> Session<A>
where
    A: StoredApplication,
    A::State: Clone,
    A::Policy: Send + 'static,
{
    pub fn new(replica: Replica<A>) -> Self {
        Self {
            replica,
            subscriptions: Mutex::new(Subscriptions {
                closed: false,
                next: 0,
                entries: BTreeMap::new(),
            }),
        }
    }
    pub fn close(&self) -> Result<(), Error> {
        let mut subscriptions = self.subscriptions.lock().map_err(|_| Error::Closed)?;
        subscriptions.closed = true;
        for entry in subscriptions.entries.values() {
            entry.cancelled.send_replace(true);
        }
        subscriptions.entries.clear();
        Ok(())
    }
    pub fn scope(&self) -> Scope {
        self.replica.inner.scope
    }
    pub async fn dispatch(&self, call: Call, coordinator: Option<&dyn Coordinator>) -> Reply {
        match self.execute(call, coordinator).await {
            Ok(value) => Reply::Ok {
                version: VERSION,
                value,
            },
            Err(error) => Reply::Error {
                version: VERSION,
                error: error.into(),
            },
        }
    }
    pub async fn dispatch_json(
        &self,
        bytes: &[u8],
        coordinator: Option<&dyn Coordinator>,
    ) -> Vec<u8> {
        let reply = if bytes.len() > MAX_FRAME_BYTES {
            Reply::Error {
                version: VERSION,
                error: Error::Quota {
                    resource: "SDK frame",
                    limit: MAX_FRAME_BYTES,
                }
                .into(),
            }
        } else {
            match serde_json::from_slice(bytes) {
                Ok(call) => self.dispatch(call, coordinator).await,
                Err(_) => Reply::Error {
                    version: VERSION,
                    error: Error::Rejected("invalid SDK call".into()).into(),
                },
            }
        };
        // Every variant contains only JSON-representable values.
        match serde_json::to_vec(&reply) {
            Ok(bytes) if bytes.len() <= MAX_FRAME_BYTES => bytes,
            _ => br#"{"status":"error","version":1,"error":{"code":"quota","retryable":false,"message":"SDK response exceeds frame budget"}}"#.to_vec(),
        }
    }
    async fn execute(
        &self,
        call: Call,
        coordinator: Option<&dyn Coordinator>,
    ) -> Result<Value, Error> {
        if self.subscriptions.lock().map_err(|_| Error::Closed)?.closed {
            return Err(Error::Closed);
        }
        if call.version != VERSION {
            return Err(Error::Incompatible("SDK version".into()));
        }
        if call.scope != self.scope() {
            return Err(Error::Rejected("SDK foreign scope".into()));
        }
        match call.request {
            Request::Read { query } => {
                let (query, options) = query.bound(call.scope)?;
                Ok(snapshot(self.replica.read(query, options).await?))
            }
            Request::ReadStrong { query } => {
                let coordinator =
                    coordinator.ok_or(Error::StrongPending(PendingReason::MissingHead))?;
                let (query, options) = query.bound(call.scope)?;
                Ok(
                    match self
                        .replica
                        .read_strong(query, options, coordinator)
                        .await?
                    {
                        StrongRead::Ready(result) => {
                            json!({"state":"ready", "snapshot":snapshot(result)})
                        }
                        StrongRead::Unavailable => json!({"state":"unavailable"}),
                        StrongRead::Pending { discovered, reason } => {
                            json!({"state":"pending", "head":discovered.head, "sequence":discovered.sequence.to_string(), "reason":pending_code(reason)})
                        }
                    },
                )
            }
            Request::Subscribe { query } => {
                let (query, options) = query.bound(call.scope)?;
                let observer = self.replica.observe_read(query, options)?;
                let mut subscriptions = self.subscriptions.lock().map_err(|_| Error::Closed)?;
                if subscriptions.entries.len() >= 64 {
                    return Err(Error::Quota {
                        resource: "SDK subscriptions",
                        limit: 64,
                    });
                }
                subscriptions.next = subscriptions.next.checked_add(1).ok_or(Error::Closed)?;
                if subscriptions.closed {
                    return Err(Error::Closed);
                }
                let id = subscriptions.next;
                let (cancelled, _) = watch::channel(false);
                subscriptions.entries.insert(
                    id,
                    Arc::new(Subscription {
                        observer: tokio::sync::Mutex::new(observer),
                        cancelled,
                    }),
                );
                Ok(json!({"subscription":id.to_string()}))
            }
            Request::Next { subscription } => {
                let id = subscription
                    .parse::<u64>()
                    .map_err(|_| Error::Rejected("invalid subscription".into()))?;
                let entry = self
                    .subscriptions
                    .lock()
                    .map_err(|_| Error::Closed)?
                    .entries
                    .get(&id)
                    .cloned()
                    .ok_or(Error::Closed)?;
                let mut cancelled = entry.cancelled.subscribe();
                if *cancelled.borrow() {
                    return Err(Error::Closed);
                }
                let mut observer = entry.observer.try_lock().map_err(|_| Error::Backpressure)?;
                tokio::select! {
                    biased;
                    _ = cancelled.changed() => Err(Error::Closed),
                    result = observer.next() => result.map(snapshot),
                }
            }
            Request::Unsubscribe { subscription } => {
                let id = subscription
                    .parse::<u64>()
                    .map_err(|_| Error::Rejected("invalid subscription".into()))?;
                if let Some(entry) = self
                    .subscriptions
                    .lock()
                    .map_err(|_| Error::Closed)?
                    .entries
                    .remove(&id)
                {
                    entry.cancelled.send_replace(true);
                }
                Ok(json!({"released":true}))
            }
            Request::Mutate {
                handle,
                entity,
                payload,
                baseline,
            } => {
                let baseline = baseline
                    .map(|value| {
                        value
                            .parse::<u64>()
                            .map(LocalRevision::new)
                            .map_err(|_| Error::Rejected("invalid revision".into()))
                    })
                    .transpose()?
                    .transpose()?;
                let result = self
                    .replica
                    .mutate_with_precondition(
                        MutationHandle(handle),
                        Mutation {
                            entity: entity.map(OperationRef::id).transpose()?,
                            payload,
                        },
                        baseline,
                    )?
                    .committed()
                    .await?;
                Ok(operation(result.id))
            }
            Request::MutationProgress { handle } => {
                let result = self.replica.lookup_mutation(MutationHandle(handle)).await?;
                Ok(match result {
                    Some(result) => json!({"state":"committed", "operation":operation(result.id)}),
                    None => json!({"state":"unresolved"}),
                })
            }
            Request::Invoke {
                handle,
                method,
                version,
                arguments,
                policy,
            } => {
                let result = self
                    .replica
                    .invoke_command(
                        CommandHandle(handle),
                        CommandInvocation {
                            method,
                            version,
                            arguments,
                            policy,
                            inputs: None,
                        },
                    )?
                    .committed()
                    .await?;
                Ok(json!({"state":"queued", "request":request_id(result.id)}))
            }
            Request::CommandProgress { handle } => {
                let progress = self
                    .replica
                    .track_command(CommandHandle(handle), &[])
                    .await?;
                let state = match progress.state {
                    None => "unresolved",
                    Some(CommandState::Queued) => "queued",
                    Some(CommandState::OutcomeUnknown) => "outcome_unknown",
                    Some(CommandState::ConfirmedLocally) => "confirmed_locally",
                    Some(CommandState::PeerApplied) => "peer_applied",
                    Some(CommandState::Released) => "released",
                    Some(CommandState::Retired) => "retired",
                    Some(CommandState::Expired) => "expired",
                };
                Ok(
                    json!({"state":state,"request":progress.request.map(request_id),"outcome":progress.outcome}),
                )
            }
        }
    }
}
fn operation(id: OperationId) -> Value {
    json!({"node":id.node.0,"sequence":id.sequence.get().to_string()})
}
fn pending_code(reason: PendingReason) -> &'static str {
    match reason {
        PendingReason::MissingInput => "missing_input",
        PendingReason::MissingConfirmedPackage => "missing_confirmed_package",
        PendingReason::Executor => "executor",
        PendingReason::MissingHead => "missing_head",
        PendingReason::StaleFence => "stale_fence",
        PendingReason::CutBase => "cut_base",
    }
}
fn snapshot(result: ReadSnapshot) -> Value {
    let completeness = match result.completeness {
        Completeness::Local { pending_operations } => {
            json!({"type":"local","pending_operations":pending_operations})
        }
        Completeness::CoordinatorHead { sequence, head } => {
            json!({"type":"coordinator_head","sequence":sequence.to_string(),"head":head})
        }
    };
    let rows: Vec<Vec<BoundValue>> = result
        .rows
        .into_iter()
        .map(|row| row.into_iter().map(BoundValue::from).collect())
        .collect();
    // `next_kind` tells the consumer what the following page costs.
    let next_kind = result.next.as_ref().map(|cursor| match cursor.kind() {
        CursorKind::Keyset => "keyset",
        CursorKind::Offset => "offset",
    });
    let next = result.next.map(|cursor| cursor.encode());
    json!({"scope":result.scope,"revision":result.revision.get().to_string(),"rows":rows,"next":next,"next_kind":next_kind,"completeness":completeness,"strong_head":{"sequence":result.strong_head.sequence.to_string(),"head":result.strong_head.head,"configuration":result.strong_head.configuration}})
}

fn request_id(id: rocky_core::RequestId) -> Value {
    json!({"node":id.node.0,"sequence":id.sequence.get().to_string()})
}

#[cfg(test)]
mod tests;
