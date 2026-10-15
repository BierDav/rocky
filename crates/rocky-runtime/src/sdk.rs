//! JSON v1 transport contract. See `docs/runtime/sdk-contract.md`.
use crate::*;
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
    pub async fn dispatch(&self, call: Call) -> Reply {
        match self.execute(call).await {
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
    pub async fn dispatch_json(&self, bytes: &[u8]) -> Vec<u8> {
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
                Ok(call) => self.dispatch(call).await,
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
    async fn execute(&self, call: Call) -> Result<Value, Error> {
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
        }
    }
}
fn operation(id: OperationId) -> Value {
    json!({"node":id.node.0,"sequence":id.sequence.get().to_string()})
}
fn snapshot(result: ReadSnapshot) -> Value {
    let completeness = match result.completeness {
        Completeness::Local { pending_operations } => {
            json!({"type":"local","pending_operations":pending_operations})
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
    json!({"scope":result.scope,"revision":result.revision.get().to_string(),"rows":rows,"next":next,"next_kind":next_kind,"completeness":completeness})
}

#[cfg(test)]
mod tests;
