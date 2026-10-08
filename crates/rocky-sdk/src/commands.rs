//! Typed strong commands on a database: the coordinator and handlers an
//! application composes, the host that runs handlers on their own thread,
//! and the JSON forms of the call contract. See
//! `docs/runtime/typed-commands.md`.
use crate::{Failure, node::Cell};
use data_encoding::HEXLOWER;
use rocky_core::{
    CommandDescriptor, CommandFailure, CommandFuture, CommandHost, Coordinator, HandlerInput,
    HandlerOutput, Outcome, SqlValue, TableWrite, TypedCommands,
};
use rocky_secure::{Ticket, TicketResult, TicketState};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, mpsc::SyncSender},
    time::Duration,
};
use tokio::sync::oneshot;

/// How long one handler call may take.
pub const HANDLER_DEADLINE: Duration = Duration::from_secs(10);
/// Handler calls that may wait for the one that runs.
const HANDLER_QUEUE: usize = 8;

/// What strong commands need besides a database. Without a handler the
/// device submits commands and reads results, and executes none.
pub struct Commands {
    pub coordinator: Coordination,
    pub handler: Option<Arc<dyn CommandHandler>>,
    /// `HANDLER_DEADLINE` unless given.
    pub deadline: Option<Duration>,
}
/// Where a database's strong commands are coordinated.
pub enum Coordination {
    /// The application's coordinator: Rocky calls it and never closes it.
    Composed(Arc<dyn Coordinator>),
    /// A coordination service its trust bundle pins, reached over the
    /// database's iroh network. The database signs with its node key and
    /// keeps receipt evidence in its directory.
    #[cfg(feature = "iroh")]
    Service(Box<rocky_coordinator::certified::TrustBundle>),
}

/// Decides commands. A call gets typed arguments and the confirmed rows of
/// the command's tables, and returns one decision. It has no database, key
/// or network; calling out or reading a clock makes replicas disagree.
/// An error is a failed call that runs again, never a business rejection:
/// reject with `Outcome::Rejected`.
pub trait CommandHandler: Send + Sync {
    fn handle(&self, input: &HandlerInput) -> Result<HandlerOutput, String>;
}

struct Call {
    input: HandlerInput,
    reply: oneshot::Sender<Result<HandlerOutput, String>>,
}
/// Runs a domain's handler on one thread of its own, so a slow or stuck
/// callback holds no database lock and no runtime thread.
pub(crate) struct Hosted {
    calls: SyncSender<Call>,
    deadline: Duration,
}
impl Hosted {
    pub fn start(
        handler: Arc<dyn CommandHandler>,
        deadline: Duration,
    ) -> Result<Arc<Self>, Failure> {
        let (calls, queue) = std::sync::mpsc::sync_channel::<Call>(HANDLER_QUEUE);
        std::thread::Builder::new()
            .name("rocky-handler".into())
            .spawn(move || {
                // Ends when the database drops its sender.
                for call in queue {
                    // The caller gave up at its deadline: nothing waits.
                    if call.reply.is_closed() {
                        continue;
                    }
                    let decision = catch_unwind(AssertUnwindSafe(|| handler.handle(&call.input)))
                        .unwrap_or_else(|_| Err("the handler panicked".into()));
                    let _ = call.reply.send(decision);
                }
            })?;
        Ok(Arc::new(Self { calls, deadline }))
    }
}
impl CommandHost for Hosted {
    fn handle(&self, input: HandlerInput) -> CommandFuture<'_, HandlerOutput> {
        Box::pin(async move {
            let retry = |reason: String| CommandFailure::Retryable(reason);
            let (reply, decision) = oneshot::channel();
            self.calls
                .try_send(Call { input, reply })
                .map_err(|_| retry("the handler is busy or stopped".into()))?;
            match tokio::time::timeout(self.deadline, decision).await {
                Ok(Ok(Ok(output))) => Ok(output),
                Ok(Ok(Err(message))) => Err(retry(message)),
                Ok(Err(_)) => Err(retry("the handler stopped".into())),
                Err(_) => Err(retry(format!(
                    "the handler passed its deadline of {} ms",
                    self.deadline.as_millis()
                ))),
            }
        })
    }
}

pub(crate) fn cells(values: Vec<SqlValue>) -> Vec<Cell> {
    values.into_iter().map(Cell::from).collect()
}
pub(crate) fn values(cells: Vec<Cell>) -> Result<Vec<SqlValue>, Failure> {
    cells.into_iter().map(Cell::sql).collect()
}
/// A handler call as the call contract spells it, for handlers written in
/// another language.
pub fn handler_input_json(input: &HandlerInput) -> String {
    json!({
        "version": 2,
        "method": input.method,
        "command_version": input.version,
        "handler": input.handler,
        "request": {
            "node": HEXLOWER.encode(&input.request.node.0),
            "sequence": input.request.sequence.get().to_string(),
        },
        "caller": HEXLOWER.encode(&input.caller.0),
        "predecessor": {
            "sequence": input.predecessor.0.to_string(),
            "head": HEXLOWER.encode(&input.predecessor.1),
        },
        "arguments": cells(input.arguments.clone()),
    })
    .to_string()
}
/// Reads only the requested row from an immutable handler snapshot.
pub fn handler_get_json(
    reads: &rocky_core::HandlerReads,
    table: &str,
    key: &str,
) -> Result<String, String> {
    let key: Vec<Cell> = serde_json::from_str(key).map_err(|e| e.to_string())?;
    let key = values(key).map_err(|e| e.to_string())?;
    Ok(json!(reads.get(table, &key)?.map(cells)).to_string())
}
/// Reads only a bounded key range from an immutable handler snapshot.
pub fn handler_scan_json(
    reads: &rocky_core::HandlerReads,
    table: &str,
    lower: Option<&str>,
    upper: Option<&str>,
    limit: usize,
) -> Result<String, String> {
    let bound = |json: &str| -> Result<Vec<SqlValue>, String> {
        let cells: Vec<Cell> = serde_json::from_str(json).map_err(|e| e.to_string())?;
        values(cells).map_err(|e| e.to_string())
    };
    let lower = lower.map(bound).transpose()?;
    let upper = upper.map(bound).transpose()?;
    Ok(json!(
        reads
            .scan(table, lower.as_deref(), upper.as_deref(), limit)?
            .into_iter()
            .map(cells)
            .collect::<Vec<_>>()
    )
    .to_string())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Decision {
    outcome: String,
    #[serde(default)]
    result: Vec<Cell>,
    #[serde(default)]
    writes: Vec<Write>,
    #[serde(default)]
    jobs: Vec<Job>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Write {
    table: String,
    #[serde(default)]
    put: Option<Vec<Cell>>,
    #[serde(default)]
    delete: Option<Vec<Cell>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Job {
    scope: String,
    payload: Vec<u8>,
}
/// The decision a foreign handler returned.
pub fn handler_output_json(json: &str) -> Result<HandlerOutput, String> {
    let invalid = |error: Failure| format!("invalid handler decision: {error}");
    if json.len() > rocky_secure::MAX_SECURE_FRAME {
        return Err("invalid handler decision: quota".into());
    }
    let decision: Decision = serde_json::from_str(json).map_err(|error| invalid(error.into()))?;
    let result = values(decision.result).map_err(invalid)?;
    let outcome = match decision.outcome.as_str() {
        "accepted" => Outcome::Accepted(result),
        "rejected" => Outcome::Rejected(result),
        _ => return Err("invalid handler decision: outcome".into()),
    };
    let mut writes = Vec::new();
    for write in decision.writes {
        writes.push(match (write.put, write.delete) {
            (Some(row), None) => TableWrite::Put {
                table: write.table,
                row: values(row).map_err(invalid)?,
            },
            (None, Some(key)) => TableWrite::Delete {
                table: write.table,
                key: values(key).map_err(invalid)?,
            },
            _ => return Err("invalid handler decision: a write puts or deletes".into()),
        });
    }
    Ok(HandlerOutput {
        outcome,
        writes,
        jobs: decision
            .jobs
            .into_iter()
            .map(|job| (job.scope, job.payload))
            .collect(),
    })
}

fn outcome(descriptor: &CommandDescriptor, bytes: &[u8]) -> Result<Value, Failure> {
    Ok(match descriptor.decode_outcome(bytes)? {
        Outcome::Accepted(result) => json!({"kind": "accepted", "values": cells(result)}),
        Outcome::Rejected(result) => json!({"kind": "rejected", "values": cells(result)}),
    })
}
/// A ticket as calls report it. The outcome is typed by the ticket's own
/// version of the command.
pub(crate) fn ticket_json(app: &TypedCommands, ticket: &Ticket) -> Result<Value, Failure> {
    let state = match ticket.state {
        TicketState::Queued => "queued",
        TicketState::Prepared => "prepared",
        TicketState::OutcomeUnknown => "outcome_unknown",
        TicketState::Confirmed => "confirmed",
        TicketState::Applied => "applied",
        TicketState::Cancelled => "cancelled",
        TicketState::Retired => "retired",
        TicketState::Expired => "expired",
    };
    let result = match &ticket.result {
        TicketResult::Pending => Value::Null,
        TicketResult::Unavailable => json!({"kind": "unavailable"}),
        TicketResult::Available(bytes) => {
            outcome(app.commands().get(&ticket.method, ticket.version)?, bytes)?
        }
    };
    let custody: Vec<Value> = ticket
        .custody
        .iter()
        .map(|custody| {
            let table = app
                .rows()
                .table(&custody.scope.table)
                .map(|table| table.sql_name.clone());
            json!({"peer": custody.peer.to_hex(), "table": table, "applied": custody.applied})
        })
        .collect();
    Ok(json!({
        "handle": HEXLOWER.encode(&ticket.handle),
        "request": {
            "node": HEXLOWER.encode(&ticket.request.node.0),
            "sequence": ticket.request.sequence.get().to_string(),
        },
        "method": ticket.method,
        "version": ticket.version,
        "state": state,
        "sequence": ticket.sequence.map(|sequence| sequence.to_string()),
        "outcome": result,
        "custody": custody,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rocky_core::{AuthorSequence, NodeId, PrincipalId, RequestId};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn input() -> HandlerInput {
        HandlerInput {
            method: "Reserve".into(),
            version: 2,
            handler: "stock.reserve.v2".into(),
            request: RequestId {
                node: NodeId([1; 16]),
                sequence: AuthorSequence::new(7).unwrap(),
            },
            caller: PrincipalId([2; 16]),
            predecessor: (4, [3; 32]),
            arguments: vec![SqlValue::Text("bolt".into()), SqlValue::Integer(i64::MAX)],
            reads: rocky_core::HandlerReads::new(
                [(
                    "stock".to_string(),
                    (vec![0], vec![vec![SqlValue::Text("bolt".into())]]),
                )]
                .into(),
            )
            .unwrap(),
        }
    }

    #[test]
    fn handler_frames_keep_integers_exact_and_refuse_unknown_shapes() {
        let frame: Value = serde_json::from_str(&handler_input_json(&input())).unwrap();
        assert_eq!(frame["command_version"], 2);
        assert_eq!(frame["predecessor"]["sequence"], "4");
        assert_eq!(
            frame["arguments"][1],
            json!({"type": "integer", "value": "9223372036854775807"})
        );
        assert!(frame.get("state").is_none());
        assert_eq!(frame["version"], 2);
        let decision = handler_output_json(
            r#"{"outcome":"accepted","result":[{"type":"integer","value":"3"}],
                "writes":[{"table":"stock","put":[{"type":"text","value":"bolt"}]},{"table":"holds","delete":[{"type":"text","value":"h"}]}],
                "jobs":[{"scope":"holds","payload":[1,2]}]}"#,
        )
        .unwrap();
        assert_eq!(
            decision.outcome,
            Outcome::Accepted(vec![SqlValue::Integer(3)])
        );
        assert_eq!(decision.writes.len(), 2);
        assert_eq!(decision.jobs, vec![("holds".to_string(), vec![1, 2])]);
        for invalid in [
            r#"{"outcome":"maybe"}"#,
            r#"{"outcome":"accepted","writes":[{"table":"stock"}]}"#,
            r#"{"outcome":"accepted","writes":[{"table":"stock","put":[],"delete":[]}]}"#,
            r#"{"outcome":"accepted","result":[{"type":"integer","value":3}]}"#,
            r#"{"outcome":"accepted","sql":"DROP TABLE stock"}"#,
            "not json",
        ] {
            assert!(handler_output_json(invalid).is_err(), "{invalid}");
        }
    }

    struct Slow {
        calls: AtomicUsize,
        release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
        threads: std::sync::Mutex<Vec<std::thread::ThreadId>>,
    }
    impl CommandHandler for Slow {
        fn handle(&self, input: &HandlerInput) -> Result<HandlerOutput, String> {
            self.threads
                .lock()
                .unwrap()
                .push(std::thread::current().id());
            match self.calls.fetch_add(1, Ordering::SeqCst) {
                // Blocks until the test releases it, past the deadline.
                0 => self
                    .release
                    .lock()
                    .unwrap()
                    .recv()
                    .map_err(|e| e.to_string())?,
                1 => panic!("the handler crashed"),
                2 => return Err("the callback threw".into()),
                _ => {}
            }
            Ok(HandlerOutput {
                outcome: Outcome::Accepted(input.arguments.clone()),
                writes: Vec::new(),
                jobs: Vec::new(),
            })
        }
    }

    #[tokio::test]
    async fn the_host_bounds_a_call_and_survives_a_stuck_or_crashing_handler() {
        let (release, blocked) = std::sync::mpsc::channel();
        let handler = Arc::new(Slow {
            calls: AtomicUsize::new(0),
            release: std::sync::Mutex::new(blocked),
            threads: Default::default(),
        });
        let host = Hosted::start(handler.clone(), Duration::from_millis(50)).unwrap();
        let retryable = |result: Result<HandlerOutput, CommandFailure>, expected: &str| match result
        {
            Err(CommandFailure::Retryable(reason)) => {
                assert!(reason.contains(expected), "{reason}")
            }
            other => panic!("not retryable: {other:?}"),
        };
        // The deadline ends the wait; the stuck callback keeps its thread.
        retryable(host.handle(input()).await, "deadline");
        // Calls queue behind it up to the bound, then are refused at once.
        for _ in 0..HANDLER_QUEUE {
            retryable(host.handle(input()).await, "deadline");
        }
        retryable(host.handle(input()).await, "busy");
        release.send(()).unwrap();
        // Abandoned calls are skipped; a crash and a failure are retryable
        // and leave the host serving, still on its one thread.
        let mut answered = None;
        for _ in 0..200 {
            match host.handle(input()).await {
                Ok(output) => {
                    answered = Some(output);
                    break;
                }
                Err(CommandFailure::Retryable(_)) => {
                    tokio::time::sleep(Duration::from_millis(5)).await
                }
                Err(other) => panic!("{other:?}"),
            }
        }
        assert_eq!(
            answered.unwrap().outcome,
            Outcome::Accepted(input().arguments)
        );
        assert!(handler.calls.load(Ordering::SeqCst) >= 4);
        let threads = handler.threads.lock().unwrap();
        assert!(threads.iter().all(|thread| *thread == threads[0]));
        assert_ne!(threads[0], std::thread::current().id());
    }
}
