//! Schema and handler generations of SDK domains: an issuer signs the
//! next generation, members fetch it, old devices keep their work, and a
//! coordinator activates a handler generation. See
//! `docs/runtime/generations.md`.
mod common;
use common::identity as support;
use common::{Device, Failure, SCHEMA, introduce};
use rocky_coordinator::MemoryCoordinator;
use rocky_core::{Activation, HandlerInput, HandlerOutput, Outcome, SqlValue, TableWrite};
use rocky_sdk::{
    CommandHandler, Commands, Coordination, Database, Network, Open, transport::memory::Hub,
};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

/// The shopping schema with a nullable note and a tags collection.
const SECOND: &str = r#"{"application":"shopping","version":2,"format":4,"tables":[{"name":"items","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"note","type":"text","not_null":false},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]},{"name":"tags","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"label","type":"text","not_null":true}],"primary_key":["key"]}]}"#;
/// Drops a field: no transition can make that.
const DROPPED: &str = r#"{"application":"shopping","version":3,"format":4,"tables":[{"name":"items","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]}]}"#;

fn open<'a>(directory: &'a Path, schema: &'a str, network: Option<Network>) -> Open<'a> {
    Open {
        directory,
        schema,
        secrets: None,
        network,
        commands: None,
    }
}
async fn generation(device: &Device) -> Value {
    device.ok(json!({"type": "generation"})).await
}
async fn rows(device: &Device, sql: &str) -> Value {
    device
        .ok(json!({"type": "read", "query": {"sql": sql, "values": [], "max_rows": 1024}}))
        .await["rows"]
        .clone()
}
fn text(value: &str) -> Value {
    json!({"type": "text", "value": value})
}

#[tokio::test(flavor = "multi_thread")]
async fn idle_old_member_still_reports_a_newer_peer_generation() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let (a_dir, b_dir) = (root.path().join("a"), root.path().join("b"));
    let a = Device::create(&a_dir, network("a"))?;
    let b = Device::join(
        &b_dir,
        &a.invite().await,
        Network::Owned(hub.connector("b")),
    )?;
    introduce(&a, &b).await;
    b.sync().await.expect("initial idle sync");
    a.close().await;
    let a = Device::new(Database::open(open(&a_dir, SECOND, network("a")))?)?;
    let report = b.sync().await.expect("idle generation sync");
    assert!(
        report["peers"]
            .as_array()
            .ok_or("peer reports")?
            .iter()
            .any(|peer| peer["upgrade_required"] == true)
    );
    b.close().await;
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn members_fetch_the_issuers_generation_and_keep_their_pending_work() -> Result<(), Failure> {
    let root = common::identity::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let (a_dir, b_dir) = (root.path().join("a"), root.path().join("b"));
    let a = Device::create(&a_dir, network("a"))?;
    let b = Device::join(
        &b_dir,
        &a.invite().await,
        Network::Owned(hub.connector("b")),
    )?;
    introduce(&a, &b).await;
    let apples = b.add(1, "apples").await;
    b.sync().await.expect("sync");
    assert_eq!(a.titles().await, ["apples"]);
    // Bread has not left the member when the issuer upgrades.
    b.add(2, "bread").await;
    a.close().await;

    let a = Device::new(Database::open(open(&a_dir, SECOND, network("a")))?)?;
    let status = generation(&a).await;
    assert_eq!(status["current"], json!({"schema": 2, "handler": 1}));
    assert_eq!(status["horizon"], 1);
    assert_eq!(status["previous"], json!([{"schema": 1, "handler": 1}]));
    assert_eq!(status["rebuilding"], Value::Null);
    assert_eq!(
        rows(&a, "SELECT title, note FROM items").await,
        json!([[text("apples"), {"type": "null"}]])
    );
    a.set(3, &apples, "note", json!("fresh")).await;
    a.ok(json!({
        "type": "mutate",
        "handle": vec![4; 32],
        "payload": json!({"type": "create", "collection": "tags", "values": {"label": "fruit"}}).to_string().into_bytes(),
        "entity": null,
        "baseline": null,
    }))
    .await;

    // The member, one generation behind, pulls; it cannot verify the
    // issuer's newer credentials, so bread waits for its upgrade.
    let synced = b.sync().await.expect("sync");
    assert!(
        synced["peers"]
            .as_array()
            .ok_or("peer reports")?
            .iter()
            .any(|peer| peer["upgrade_required"] == true)
    );
    assert_eq!(a.titles().await, ["apples"]);
    assert_eq!(b.titles().await, ["apples", "bread"]);
    b.close().await;

    // Without a network the member cannot fetch the generation; nothing
    // changes and the old schema still opens.
    let offline = Database::open(open(&b_dir, SECOND, None));
    let error = offline.err().expect("needs a network").to_string();
    assert!(error.contains("network"), "{error}");
    let b = Device::new(Database::open(open(&b_dir, SCHEMA, None))?)?;
    assert_eq!(b.titles().await, ["apples", "bread"]);
    b.close().await;

    let b = Device::new(Database::open(open(&b_dir, SECOND, network("b")))?)?;
    assert_eq!(generation(&b).await["current"]["schema"], 2);
    // Its pending generation 1 write is delivered with its original bytes.
    b.sync().await.expect("sync");
    assert_eq!(a.titles().await, ["apples", "bread"]);
    assert_eq!(
        rows(&b, "SELECT note FROM items WHERE title='apples'").await,
        json!([[text("fresh")]])
    );
    // Its profile names `items` only; a new collection is not granted.
    let denied = b
        .call(json!({"type": "read", "query": {"sql": "SELECT label FROM tags", "values": [], "max_rows": 16}}))
        .await
        .expect_err("not granted");
    assert_eq!(denied["code"], "rejected");
    assert!(
        denied["message"]
            .as_str()
            .unwrap()
            .contains("no such table")
    );
    // A device joining now admits the domain's whole history.
    let invitation = a
        .invite_as(json!({"kind": "reader", "scopes": ["items", "tags"]}))
        .await;
    let c_dir = root.path().join("c");
    let c = Device::new(Database::join(
        open(&c_dir, SECOND, network("c")),
        &invitation,
    )?)?;
    introduce(&a, &c).await;
    c.sync().await.expect("sync");
    assert_eq!(c.titles().await, ["apples", "bread"]);
    assert_eq!(
        rows(&c, "SELECT label FROM tags").await,
        json!([[text("fruit")]])
    );
    c.close().await;
    b.close().await;

    // A change no transition can make is refused before anything changes.
    a.close().await;
    let refused = Database::open(open(&a_dir, DROPPED, network("a")));
    let error = refused.err().expect("refused").to_string();
    assert!(error.contains("removed"), "{error}");
    let a = Device::new(Database::open(open(&a_dir, SECOND, network("a")))?)?;
    assert_eq!(generation(&a).await["current"]["schema"], 2);
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_local_schema_change_rebuilds_tables_and_keeps_the_generation() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let a = Device::create(root.path(), None)?;
    a.add(1, "apples").await;
    a.close().await;
    // The application's own version is local: the manifest stays.
    let bumped = SCHEMA.replace(r#""version":1"#, r#""version":7"#);
    let bumped: &'static str = Box::leak(bumped.into_boxed_str());
    let a = Device::new(Database::open(open(root.path(), bumped, None))?)?;
    let status = generation(&a).await;
    assert_eq!(status["current"], json!({"schema": 1, "handler": 1}));
    assert_eq!(status["previous"], json!([]));
    assert_eq!(a.titles().await, ["apples"]);
    a.close().await;
    Ok(())
}

const COMMANDS: &str = r#"{"application":"counter","version":1,"commands":{"format":1,"handler":1,"commands":[{"name":"Add","version":1,"handler":"counts.add.v1","arguments":[{"name":"amount","type":"integer"}],"accepted":[{"name":"value","type":"integer"}],"rejected":[],"invocation":"counts","result":"counts","resources":["counts"]}]},"format":4,"tables":[{"name":"counts","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"value","type":"integer","not_null":true}],"primary_key":["key"]}]}"#;

/// Adds to one counter; `factor` stands for the handler's implementation.
struct Counter {
    factor: i64,
}
impl CommandHandler for Counter {
    fn handle(&self, input: &HandlerInput) -> Result<HandlerOutput, String> {
        let SqlValue::Integer(amount) = input.arguments[0] else {
            return Err("amount".into());
        };
        let value = match input
            .reads
            .scan("counts", None, None, 1)?
            .first()
            .map(|row| &row[1])
        {
            Some(SqlValue::Integer(value)) => *value,
            _ => 0,
        } + amount * self.factor;
        Ok(HandlerOutput {
            outcome: Outcome::Accepted(vec![SqlValue::Integer(value)]),
            writes: vec![TableWrite::Put {
                table: "counts".into(),
                row: support::identity_row(vec![
                    SqlValue::Text("total".into()),
                    SqlValue::Integer(value),
                ]),
            }],
            jobs: Vec::new(),
        })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_handler_generation_runs_only_after_the_coordinator_activates_it() -> Result<(), Failure>
{
    let root = tempfile::tempdir()?;
    let coordinator = Arc::new(MemoryCoordinator::default());
    let commands = |factor| {
        Some(Commands {
            coordinator: Coordination::Composed(coordinator.clone()),
            handler: Some(Arc::new(Counter { factor }) as Arc<dyn CommandHandler>),
            deadline: None,
        })
    };
    let directory = root.path().join("a");
    let a = Device::new(Database::create(
        Open {
            directory: &directory,
            schema: COMMANDS,
            secrets: None,
            network: None,
            commands: commands(1),
        },
        None,
    )?)?;
    let identity = a.ok(json!({"type": "command_identity"})).await;
    let id = |name: &str| -> [u8; 16] {
        let hex = identity[name].as_str().unwrap_or_default();
        let bytes: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap_or_default())
            .collect();
        bytes.try_into().unwrap_or_default()
    };
    let (node, owner) = (
        rocky_core::NodeId(id("node")),
        rocky_core::PrincipalId(id("principal")),
    );
    let scope: rocky_core::Scope = serde_json::from_value(identity["scope"].clone())?;
    coordinator.register(scope, owner, [1; 16], vec![node], 1, 1)?;
    coordinator.acquire(scope, owner, node)?;
    let add = |handle: u8, amount: i64| {
        json!({"type": "command_submit", "handle": vec![handle; 32], "method": "Add", "version": 1,
               "arguments": [{"type": "integer", "value": amount.to_string()}]})
    };
    let run = |handle: u8| json!({"type": "command_run", "handle": vec![handle; 32]});
    a.ok(add(1, 2)).await;
    assert_eq!(a.ok(run(1)).await["tickets"][0]["state"], "applied");
    // Submitted under handler generation 1, run after the upgrade.
    a.ok(add(2, 3)).await;
    a.close().await;

    let upgraded = COMMANDS.replace(r#""handler":1,"commands""#, r#""handler":2,"commands""#);
    let upgraded: &'static str = Box::leak(upgraded.into_boxed_str());
    let a = Device::new(Database::open(Open {
        directory: &directory,
        schema: upgraded,
        secrets: None,
        network: None,
        commands: commands(10),
    })?)?;
    assert_eq!(
        generation(&a).await["current"],
        json!({"schema": 1, "handler": 2})
    );
    // The coordinator still runs generation 1: the new handler waits.
    let waiting = a.ok(run(2)).await["tickets"][0].clone();
    assert_eq!(waiting["state"], "queued");
    assert_eq!(waiting["error"]["code"], "unsupported_generation");
    assert_eq!(waiting["error"]["retryable"], true);
    let proposal = a.ok(json!({"type": "command_activation"})).await;
    assert_eq!(proposal["active"], false);
    let activation: Activation = serde_json::from_value(proposal["activation"].clone())?;
    assert_eq!((activation.schema, activation.handler), (1, 2));
    assert_eq!(activation.head.sequence, 1);
    // Only the owner activates, at exactly this head.
    assert!(
        coordinator
            .activate(rocky_core::PrincipalId([0; 16]), &activation)
            .is_err()
    );
    let head = coordinator.activate(owner, &activation)?;
    assert_eq!((head.schema, head.handler), (1, 2));
    assert_eq!(
        a.ok(json!({"type": "command_activation"})).await["active"],
        true
    );
    let ran = a.ok(run(2)).await["tickets"][0].clone();
    assert_eq!(ran["state"], "applied", "{ran}");
    assert_eq!(
        ran["outcome"],
        json!({"kind": "accepted", "values": [{"type": "integer", "value": "32"}]})
    );
    a.close().await;
    Ok(())
}
