//! Typed strong commands composed on databases: durable tickets, a handler
//! on its own thread, delivery over the attached network or as bytes an
//! application forwards, and results that follow sync. See
//! `docs/runtime/typed-commands.md`.
mod support;
use rocky_coordinator::MemoryCoordinator;
use rocky_core::{HandlerInput, HandlerOutput, Outcome, Scope, SqlValue, TableWrite};
use rocky_sdk::{
    CommandHandler, Commands, Database, Network, Open, Opened, Session, transport::memory::Hub,
};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

type Failure = Box<dyn std::error::Error + Send + Sync>;

const SCHEMA: &str = r#"{"application":"reservations","version":1,"commands":{"format":1,"handler":1,"commands":[{"name":"Restock","version":1,"handler":"stock.restock.v1","arguments":[{"name":"item","type":"text"},{"name":"quantity","type":"integer"}],"accepted":[{"name":"remaining","type":"integer"}],"rejected":[],"invocation":"stock","result":"stock","resources":["stock","holds"]},{"name":"Reserve","version":1,"handler":"stock.reserve.v1","arguments":[{"name":"item","type":"text"},{"name":"quantity","type":"integer"},{"name":"hold","type":"text"}],"accepted":[{"name":"remaining","type":"integer"}],"rejected":[{"name":"remaining","type":"integer"},{"name":"requested","type":"integer"}],"invocation":"holds","result":"holds","resources":["stock","holds"]}]},"format":4,"tables":[{"name":"holds","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"item","type":"text","not_null":true},{"name":"quantity","type":"integer","not_null":true}],"primary_key":["key"]},{"name":"stock","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"remaining","type":"integer","not_null":true}],"primary_key":["key"]}]}"#;

/// Stock never goes below zero. `stall` makes the next call outlast any
/// deadline until the sender is dropped.
#[derive(Default)]
struct Reservations {
    calls: AtomicUsize,
    stall: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
}
impl CommandHandler for Reservations {
    fn handle(&self, input: &HandlerInput) -> Result<HandlerOutput, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let stall = self.stall.lock().map_err(|e| e.to_string())?.take();
        if let Some(stall) = stall {
            let _ = stall.recv();
        }
        let number = |value: &SqlValue| match value {
            SqlValue::Integer(number) => Ok(*number),
            _ => Err("not a number".to_string()),
        };
        let item = input.arguments[0].clone();
        let quantity = number(&input.arguments[1])?;
        // A stock row is its `id` and `remaining`.
        let remaining = match input.reads.get("stock", std::slice::from_ref(&item))? {
            Some(row) => number(&row[1])?,
            None => 0,
        };
        let stock = |remaining| TableWrite::Put {
            table: "stock".into(),
            row: support::identity_row(vec![item.clone(), SqlValue::Integer(remaining)]),
        };
        Ok(if input.method == "Restock" {
            HandlerOutput {
                outcome: Outcome::Accepted(vec![SqlValue::Integer(remaining + quantity)]),
                writes: vec![stock(remaining + quantity)],
                jobs: Vec::new(),
            }
        } else if quantity > remaining {
            HandlerOutput {
                outcome: Outcome::Rejected(vec![
                    SqlValue::Integer(remaining),
                    SqlValue::Integer(quantity),
                ]),
                writes: Vec::new(),
                jobs: Vec::new(),
            }
        } else {
            HandlerOutput {
                outcome: Outcome::Accepted(vec![SqlValue::Integer(remaining - quantity)]),
                writes: vec![
                    stock(remaining - quantity),
                    TableWrite::Put {
                        table: "holds".into(),
                        row: support::identity_row(vec![
                            input.arguments[2].clone(),
                            item.clone(),
                            SqlValue::Integer(quantity),
                        ]),
                    },
                ],
                jobs: Vec::new(),
            }
        })
    }
}

struct Device {
    database: Arc<Database>,
    session: Session,
    scope: Value,
}
impl Device {
    fn new(opened: Opened) -> Result<Self, Failure> {
        Ok(Self {
            session: opened.database.session(),
            scope: serde_json::from_str(&opened.database.scope())?,
            database: opened.database,
        })
    }
    async fn call(&self, request: Value) -> Result<Value, Value> {
        let call = json!({"version": 1, "scope": self.scope, "request": request});
        let reply = self
            .session
            .call(call.to_string())
            .await
            .map_err(|error| json!(error.to_string()))?;
        let reply: Value =
            serde_json::from_str(&reply).map_err(|error| json!(error.to_string()))?;
        if reply["status"] == "ok" {
            Ok(reply["value"].clone())
        } else {
            Err(reply["error"].clone())
        }
    }
    async fn ok(&self, request: Value) -> Value {
        match self.call(request.clone()).await {
            Ok(value) => value,
            Err(error) => panic!("{request} failed: {error}"),
        }
    }
    async fn submit(&self, handle: u8, method: &str, arguments: Value) -> Value {
        self.ok(json!({"type": "command_submit", "handle": vec![handle; 32], "method": method, "version": 1, "arguments": arguments}))
            .await
    }
    async fn reserve(&self, handle: u8, quantity: i64) -> Value {
        self.submit(
            handle,
            "Reserve",
            json!([
                text("bolt"),
                integer(quantity),
                text(&format!("hold-{handle}"))
            ]),
        )
        .await
    }
    /// Runs one ticket and returns it with the `error` of that run.
    async fn run(&self, handle: u8) -> Value {
        self.ok(json!({"type": "command_run", "handle": vec![handle; 32]}))
            .await["tickets"][0]
            .clone()
    }
    async fn ticket(&self, handle: u8) -> Value {
        self.ok(json!({"type": "command_ticket", "handle": vec![handle; 32]}))
            .await
    }
    async fn rows(&self, table: &str) -> Value {
        self.ok(json!({"type": "command_rows", "table": table}))
            .await
    }
    async fn identity(&self) -> (rocky_core::NodeId, rocky_core::PrincipalId, Scope) {
        let identity = self.ok(json!({"type": "command_identity"})).await;
        let id = |name: &str| -> [u8; 16] {
            let hex = identity[name].as_str().expect("hex");
            let bytes: Vec<u8> = (0..hex.len())
                .step_by(2)
                .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).expect("hex"))
                .collect();
            bytes.try_into().expect("16 bytes")
        };
        (
            rocky_core::NodeId(id("node")),
            rocky_core::PrincipalId(id("principal")),
            serde_json::from_value(identity["scope"].clone()).expect("scope"),
        )
    }
    async fn close(&self) {
        self.database.close().await.expect("close");
    }
}
fn text(value: &str) -> Value {
    json!({"type": "text", "value": value})
}
fn integer(value: i64) -> Value {
    json!({"type": "integer", "value": value.to_string()})
}
fn open<'a>(directory: &'a Path, network: Option<Network>, commands: Option<Commands>) -> Open<'a> {
    Open {
        directory,
        schema: SCHEMA,
        secrets: None,
        network,
        commands,
    }
}
fn commands(
    coordinator: &Arc<MemoryCoordinator>,
    handler: Option<&Arc<Reservations>>,
    deadline: Option<Duration>,
) -> Option<Commands> {
    Some(Commands {
        coordinator: rocky_sdk::Coordination::Composed(coordinator.clone()),
        handler: handler.map(|handler| handler.clone() as Arc<dyn CommandHandler>),
        deadline,
    })
}
/// The creator `a`, which executes, and a writer `b` that only invokes,
/// both on one coordinator, as a provider's shared one would be.
struct Pair {
    root: tempfile::TempDir,
    hub: Arc<Hub>,
    coordinator: Arc<MemoryCoordinator>,
    handler: Arc<Reservations>,
    a: Device,
    b: Device,
    scope: Scope,
}
impl Pair {
    async fn new(deadline: Option<Duration>) -> Result<Self, Failure> {
        let root = tempfile::tempdir()?;
        let hub = Arc::new(Hub::default());
        let coordinator = Arc::new(MemoryCoordinator::default());
        let handler = Arc::new(Reservations::default());
        let a = Device::new(Database::create(
            open(
                &root.path().join("a"),
                Some(Network::Owned(hub.connector("a"))),
                commands(&coordinator, Some(&handler), deadline),
            ),
            None,
        )?)?;
        let invitation = a
            .ok(json!({"type": "pairing", "profile": {"kind": "writer", "scopes": ["holds", "stock"]}}))
            .await["uri"]
            .as_str()
            .ok_or("uri")?
            .to_owned();
        let b = Device::new(Database::join(
            open(
                &root.path().join("b"),
                Some(Network::Owned(hub.connector("b"))),
                commands(&coordinator, None, None),
            ),
            &invitation,
        )?)?;
        // Provisioning the coordinator is the application's or provider's
        // step: who may execute, and who may ask.
        let (executor, owner, scope) = a.identity().await;
        let (caller, principal, _) = b.identity().await;
        coordinator.register(scope, owner, [1; 16], vec![executor], 1, 1)?;
        coordinator.set_invokers(scope, owner, vec![(executor, owner), (caller, principal)])?;
        coordinator.acquire(scope, owner, executor)?;
        Ok(Self {
            root,
            hub,
            coordinator,
            handler,
            a,
            b,
            scope,
        })
    }
    async fn restock(&self, handle: u8, quantity: i64) {
        self.a
            .submit(handle, "Restock", json!([text("bolt"), integer(quantity)]))
            .await;
        assert_eq!(self.a.run(handle).await["state"], "applied");
    }
    fn sequence(&self) -> u64 {
        self.coordinator.head(self.scope).expect("head").sequence
    }
}

/// An executor acknowledging before another device syncs leaves that
/// device a result it still verifies: nothing surfaces as a failed pass.
#[tokio::test(flavor = "multi_thread")]
async fn acknowledging_before_a_reader_syncs_keeps_its_results_verifiable() -> Result<(), Failure> {
    let pair = Pair::new(None).await?;
    pair.restock(1, 2).await;
    pair.b.ok(json!({"type": "sync"})).await;
    // The reader holds a base; the next result arrives by the results pull.
    pair.restock(2, 1).await;
    pair.a.ok(json!({"type": "command_acknowledge"})).await;
    pair.b.ok(json!({"type": "sync"})).await;
    let snapshot = pair.b.rows("stock").await;
    assert_eq!(snapshot["complete"], json!(true), "{snapshot}");
    assert_eq!(snapshot["rows"][0][1], integer(3), "{snapshot}");
    let activity = pair.b.ok(json!({"type": "activity"})).await;
    assert!(
        activity["events"]
            .as_array()
            .ok_or("events")?
            .iter()
            .all(|event| event["kind"] != "results_failed"),
        "{activity}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn competing_callers_get_one_acceptance_over_the_network() -> Result<(), Failure> {
    let pair = Pair::new(None).await?;
    let (a, b) = (&pair.a, &pair.b);
    pair.restock(1, 1).await;

    // Submission is local: it returns a ticket and contacts nobody.
    let theirs = b.reserve(2, 1).await;
    let mine = a.reserve(3, 1).await;
    assert_eq!(
        (&theirs["state"], &mine["state"]),
        (&json!("queued"), &json!("queued"))
    );
    assert_eq!(theirs["outcome"], Value::Null);
    assert_eq!(b.reserve(2, 1).await, theirs);
    assert_eq!(
        b.call(json!({"type": "command_submit", "handle": vec![2; 32], "method": "Reserve", "version": 1, "arguments": [text("bolt"), integer(2), text("hold-2")]}))
            .await
            .unwrap_err()["code"],
        "conflict"
    );
    assert_eq!(pair.sequence(), 1);

    // The executor runs its own ticket; the other is delivered to it.
    let mine = a.run(3).await;
    assert_eq!(mine["state"], "applied");
    assert_eq!(
        mine["outcome"],
        json!({"kind": "accepted", "values": [integer(0)]})
    );
    let theirs = b.run(2).await;
    assert_eq!(theirs["error"], Value::Null);
    // A checkpoint may already supply predecessors; otherwise sync completes them.
    assert!(
        theirs["state"] == "confirmed" || theirs["state"] == "applied",
        "{theirs}"
    );
    assert_eq!(theirs["sequence"], "3");
    assert_eq!(
        theirs["outcome"],
        json!({"kind": "rejected", "values": [integer(0), integer(1)]})
    );
    b.ok(json!({"type": "sync"})).await;
    assert_eq!(b.ticket(2).await["state"], "applied");
    for device in [a, b] {
        let stock = device.rows("stock").await;
        assert_eq!(stock["complete"], true);
        assert_eq!(
            stock["columns"],
            json!(["key", "remaining", "id", "row_kind"])
        );
        assert_eq!(
            stock["rows"],
            json!([[text("bolt"), integer(0), integer(4295801317), integer(0)]])
        );
        assert_eq!(
            device.rows("holds").await["rows"],
            json!([[
                text("hold-3"),
                text("bolt"),
                integer(1),
                integer(4295949685),
                integer(0)
            ]])
        );
    }
    // The executor learned that the caller holds and applied the results.
    let custody = a.ticket(3).await["custody"].clone();
    assert!(
        custody
            .as_array()
            .is_some_and(|acks| acks.len() == 2 && acks.iter().all(|ack| ack["applied"] == true)),
        "{custody}"
    );
    assert_eq!(pair.handler.calls.load(Ordering::SeqCst), 3);
    assert_eq!(pair.sequence(), 3);
    assert_eq!(
        a.ok(json!({"type": "command_tickets"})).await["tickets"],
        json!([])
    );

    // Confirmed command rows are not the mergeable table of the same name.
    let page = a
        .ok(json!({"type": "read", "query": {"sql": "SELECT key FROM stock", "values": [], "max_rows": 8}}))
        .await;
    assert_eq!(page["rows"], json!([]));

    // Definite refusals, each before anything is stored.
    for (request, code) in [
        (
            json!({"type": "command_submit", "handle": vec![9; 32], "method": "Reserve", "version": 2, "arguments": []}),
            "rejected",
        ),
        (
            json!({"type": "command_submit", "handle": vec![9; 32], "method": "Drain", "version": 1, "arguments": []}),
            "rejected",
        ),
        (
            json!({"type": "command_submit", "handle": vec![9; 32], "method": "Reserve", "version": 1, "arguments": [text("bolt"), text("one"), text("h")]}),
            "rejected",
        ),
        (
            json!({"type": "command_submit", "handle": vec![9; 16], "method": "Reserve", "version": 1, "arguments": []}),
            "rejected",
        ),
    ] {
        let error = b.call(request).await.unwrap_err();
        assert_eq!(error["code"], code, "{error}");
        assert_eq!(error["retryable"], false);
    }
    assert_eq!(b.ticket(9).await, json!({"state": "unresolved"}));
    a.close().await;
    b.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn tickets_survive_reopen_lost_replies_and_forwarding_by_the_application()
-> Result<(), Failure> {
    let pair = Pair::new(None).await?;
    let (a, b) = (&pair.a, &pair.b);
    pair.restock(1, 3).await;

    // The coordinator accepts and its answer is lost: the outcome is open.
    let submitted = a.reserve(2, 1).await;
    pair.coordinator.drop_next_reply()?;
    let unknown = a.run(2).await;
    assert_eq!(unknown["state"], "outcome_unknown");
    assert_eq!(unknown["error"]["code"], "unknown_confirmation");
    assert_eq!(unknown["error"]["retryable"], true);
    // Cancelling stops nothing that may already be confirmed.
    assert_eq!(
        a.ok(json!({"type": "command_cancel", "handle": vec![2; 32]}))
            .await["state"],
        "outcome_unknown"
    );
    let calls = pair.handler.calls.load(Ordering::SeqCst);
    a.close().await;
    let a = Device::new(Database::open(open(
        &pair.root.path().join("a"),
        Some(Network::Owned(pair.hub.connector("a"))),
        commands(&pair.coordinator, Some(&pair.handler), None),
    ))?)?;
    let reopened = a.ticket(2).await;
    assert_eq!(reopened["state"], "outcome_unknown");
    assert_eq!(reopened["request"], submitted["request"]);
    let recovered = a.run(2).await;
    assert_eq!(recovered["state"], "applied");
    assert_eq!(recovered["request"], submitted["request"]);
    assert_eq!(
        recovered["outcome"],
        json!({"kind": "accepted", "values": [integer(2)]})
    );
    // The retained preparation was confirmed: no second decision.
    assert_eq!(pair.handler.calls.load(Ordering::SeqCst), calls);
    assert_eq!(pair.sequence(), 2);

    // A ticket that never left the device is withdrawn and never runs.
    a.reserve(3, 1).await;
    assert_eq!(
        a.ok(json!({"type": "command_cancel", "handle": vec![3; 32]}))
            .await["state"],
        "cancelled"
    );
    assert_eq!(a.run(3).await["error"]["code"], "rejected");
    assert_eq!(pair.sequence(), 2);

    // Without Rocky's network the application forwards the bytes itself:
    // envelope, execute, settle. The executor checks the same signature.
    let b = {
        b.close().await;
        Device::new(Database::open(open(
            &pair.root.path().join("b"),
            None,
            commands(&pair.coordinator, None, None),
        ))?)?
    };
    b.reserve(4, 1).await;
    let stranded = b.run(4).await;
    assert_eq!(stranded["error"]["code"], "network_required");
    assert_eq!(stranded["state"], "queued");
    let envelope = b
        .ok(json!({"type": "command_envelope", "handle": vec![4; 32]}))
        .await["envelope"]
        .clone();
    assert_eq!(b.ticket(4).await["state"], "outcome_unknown");
    let mut forged: Vec<u8> = serde_json::from_value(envelope.clone())?;
    let last = forged.len() - 1;
    forged[last] ^= 1;
    for bytes in [json!(forged), json!([1, 2, 3])] {
        let refusal = a
            .ok(json!({"type": "command_execute", "envelope": bytes}))
            .await["answer"]
            .clone();
        assert_eq!(
            b.ok(json!({"type": "command_settle", "handle": vec![4; 32], "answer": refusal}))
                .await["state"],
            "queued"
        );
    }
    assert_eq!(pair.sequence(), 2);
    let answer = a
        .ok(json!({"type": "command_execute", "envelope": envelope}))
        .await["answer"]
        .clone();
    // An answer for another ticket settles nothing.
    b.reserve(5, 1).await;
    assert!(
        b.call(json!({"type": "command_settle", "handle": vec![5; 32], "answer": answer}))
            .await
            .is_err()
    );
    let settled = b
        .ok(json!({"type": "command_settle", "handle": vec![4; 32], "answer": answer}))
        .await;
    assert_eq!(settled["state"], "confirmed");
    assert_eq!(
        settled["outcome"],
        json!({"kind": "accepted", "values": [integer(1)]})
    );
    assert_eq!(pair.sequence(), 3);
    // Delivering the same envelope again confirms nothing new.
    let again = a
        .ok(json!({"type": "command_execute", "envelope": envelope}))
        .await["answer"]
        .clone();
    assert_eq!(
        b.ok(json!({"type": "command_settle", "handle": vec![4; 32], "answer": again}))
            .await["sequence"],
        "3"
    );
    assert_eq!(pair.sequence(), 3);
    a.close().await;
    b.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stuck_handler_times_out_and_the_ticket_runs_again() -> Result<(), Failure> {
    let pair = Pair::new(Some(Duration::from_millis(100))).await?;
    let a = &pair.a;
    pair.restock(1, 2).await;
    a.reserve(2, 1).await;
    let (release, stall) = std::sync::mpsc::channel::<()>();
    *pair.handler.stall.lock().map_err(|e| e.to_string())? = Some(stall);
    let timed_out = a.run(2).await;
    assert_eq!(timed_out["error"]["code"], "handler_unavailable");
    assert_eq!(timed_out["error"]["retryable"], true);
    assert_eq!(timed_out["state"], "queued");
    // The database answers other calls while the callback is stuck.
    assert_eq!(
        a.rows("stock").await["rows"],
        json!([[text("bolt"), integer(2), integer(4295801317), integer(0)]])
    );
    assert_eq!(pair.sequence(), 1);
    drop(release);
    let mut ticket = a.run(2).await;
    for _ in 0..100 {
        if ticket["state"] == "applied" {
            break;
        }
        // The stalled call still holds the handler's one thread.
        assert_eq!(ticket["error"]["retryable"], true, "{ticket}");
        tokio::time::sleep(Duration::from_millis(20)).await;
        ticket = a.run(2).await;
    }
    assert_eq!(ticket["state"], "applied");
    assert_eq!(
        ticket["outcome"],
        json!({"kind": "accepted", "values": [integer(1)]})
    );
    assert_eq!(pair.sequence(), 2);

    // A database opened without commands holds definitions only.
    a.close().await;
    let bare = Device::new(Database::open(open(
        &pair.root.path().join("a"),
        None,
        None,
    ))?)?;
    assert_eq!(bare.ticket(2).await["state"], "applied");
    bare.reserve(3, 1).await;
    assert_eq!(bare.run(3).await["error"]["code"], "rejected");
    assert_eq!(bare.ticket(3).await["state"], "queued");
    bare.close().await;
    pair.b.close().await;
    Ok(())
}

/// Measures strong-command latency over growing confirmed history with
/// constant state. Run with `-- --ignored --nocapture`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "measurement"]
async fn command_latency_does_not_grow_with_confirmed_history() -> Result<(), Failure> {
    let pair = Pair::new(None).await?;
    let handle = |n: u32| {
        let mut h = vec![0u8; 32];
        h[..4].copy_from_slice(&n.to_le_bytes());
        h
    };
    let mut windows = Vec::new();
    let mut started = std::time::Instant::now();
    for n in 0..400u32 {
        // Two of three commands are delivered by the invoking device.
        let device = if n % 3 == 0 { &pair.a } else { &pair.b };
        device.ok(json!({"type": "command_submit", "handle": handle(n), "method": "Restock", "version": 1, "arguments": [text("bolt"), integer(1)]})).await;
        let ticket = device
            .ok(json!({"type": "command_run", "handle": handle(n)}))
            .await;
        let state = &ticket["tickets"][0]["state"];
        assert!(state == "applied" || state == "confirmed", "{ticket}");
        if n % 10 == 9 {
            pair.a.ok(json!({"type": "command_acknowledge"})).await;
            pair.b.ok(json!({"type": "command_acknowledge"})).await;
        }
        if n % 100 == 99 {
            windows.push(started.elapsed() / 100);
            started = std::time::Instant::now();
        }
    }
    eprintln!("per command, by hundred confirmed: {windows:?}");
    assert!(windows[3] < windows[0] * 2, "{windows:?}");
    Ok(())
}

/// Reopen and first-command timing with a fixed strong window, matching the
/// 2026-10-07 in-process release probe's two-device profile.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "measurement"]
async fn first_command_after_reopen_is_bounded_by_strong_window() -> Result<(), Failure> {
    let mut pair = Pair::new(None).await?;
    for device in [&pair.a, &pair.b] {
        for collection in ["stock", "holds"] {
            device.ok(json!({"type":"retention","collection":collection,"policy":{"required":[],"behind":[],"copies":1,"window":16}})).await;
        }
    }
    let handle = |n: u32| {
        let mut h = vec![0u8; 32];
        h[..4].copy_from_slice(&n.to_le_bytes());
        h
    };
    let mut measurements = Vec::new();
    for n in 1..=500u32 {
        let submitted = std::time::Instant::now();
        pair.b.ok(json!({"type":"command_submit","handle":handle(n),"method":"Restock","version":1,"arguments":[text("bolt"),integer(1)]})).await;
        let ticket = pair
            .b
            .ok(json!({"type":"command_run","handle":handle(n)}))
            .await;
        assert_eq!(ticket["tickets"][0]["error"], Value::Null, "{ticket}");
        if n % 100 == 1 && n > 1 {
            measurements.push((n - 1, submitted.elapsed()));
        }
        if n % 10 == 0 {
            pair.b.ok(json!({"type":"sync"})).await;
            pair.a.ok(json!({"type":"command_acknowledge"})).await;
            pair.b.ok(json!({"type":"command_acknowledge"})).await;
        }
        if n % 100 == 0 && n < 500 {
            pair.b.close().await;
            let started = std::time::Instant::now();
            pair.b = Device::new(Database::open(open(
                &pair.root.path().join("b"),
                Some(Network::Owned(pair.hub.connector("b"))),
                commands(&pair.coordinator, None, None),
            ))?)?;
            let opened = started.elapsed();
            let started = std::time::Instant::now();
            pair.b.ok(json!({"type":"sync"})).await;
            eprintln!(
                "reopen at {n}: open={opened:?}, first_sync={:?}",
                started.elapsed()
            );
        }
    }
    eprintln!("window=16; first command after reopen by confirmed prefix: {measurements:?}");
    assert_eq!(measurements.len(), 4);
    assert!(
        measurements.last().unwrap().1 < measurements[0].1 * 3,
        "{measurements:?}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reader_behind_pruned_strong_history_bootstraps_then_confirms_a_command()
-> Result<(), Failure> {
    let mut pair = Pair::new(None).await?;
    for collection in ["stock", "holds"] {
        pair.a.ok(json!({"type":"retention","collection":collection,"policy":{"required":[],"behind":[],"copies":1,"window":2}})).await;
    }
    pair.b.close().await;
    for n in 1..=12u8 {
        pair.restock(n, 1).await;
        if n % 4 == 0 {
            pair.a.ok(json!({"type":"command_acknowledge"})).await;
        }
    }
    pair.b = Device::new(Database::open(open(
        &pair.root.path().join("b"),
        Some(Network::Owned(pair.hub.connector("b"))),
        commands(&pair.coordinator, None, None),
    ))?)?;
    pair.b.ok(json!({"type":"sync"})).await;
    let snapshot = pair.b.rows("stock").await;
    assert_eq!(snapshot["complete"], json!(true), "{snapshot}");
    pair.b
        .submit(100, "Restock", json!([text("bolt"), integer(1)]))
        .await;
    let ticket = pair.b.run(100).await;
    assert_eq!(ticket["error"], Value::Null, "{ticket}");
    assert_eq!(pair.sequence(), 13);
    Ok(())
}
