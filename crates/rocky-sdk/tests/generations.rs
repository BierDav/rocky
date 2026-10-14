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
const SECOND: &str = r#"{"application":"shopping","version":2,"format":6,"tables":[{"name":"items","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"note","type":"text","not_null":false},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]},{"name":"tags","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"label","type":"text","not_null":true}],"primary_key":["key"]}]}"#;
/// The second schema, with `tags` granted like `items`.
const GRANTED: &str = r#"{"application":"shopping","version":2,"format":6,"tables":[{"name":"items","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"note","type":"text","not_null":false},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]},{"name":"tags","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"label","type":"text","not_null":true}],"primary_key":["key"]}],"granted_like":{"tags":"items"}}"#;
/// Requires `position`, which a migration computes from the titles.
const REQUIRED: &str = r#"{"application":"shopping","version":2,"format":6,"tables":[{"name":"items","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"position","type":"text","not_null":true,"default":""},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]}],"migrations":[{"version":2,"statements":["UPDATE items SET position = printf('%04d', (SELECT count(*) FROM items AS earlier WHERE earlier.title < items.title))"]}]}"#;
/// `position` nullable, and `BARE` requiring it without a migration.
const NULLABLE: &str = r#"{"application":"shopping","version":1,"format":6,"tables":[{"name":"items","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"position","type":"text","not_null":false},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]}]}"#;
const BARE: &str = r#"{"application":"shopping","version":2,"format":6,"tables":[{"name":"items","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"position","type":"text","not_null":true,"default":""},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]}]}"#;
/// Titles are unique, the later write winning: requiring a field rebuilds
/// a constrained projection.
const UNIQUE: &str = r#"{"application":"shopping","version":1,"format":6,"tables":[{"name":"items","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"title","type":"text","not_null":true}],"primary_key":["key"],"indexes":[{"name":"items_title","unique":true,"columns":[{"name":"title","descending":false}],"on_conflict":"latest_wins"}]}]}"#;
const UNIQUE_REQUIRED: &str = r#"{"application":"shopping","version":2,"format":6,"tables":[{"name":"items","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"position","type":"text","not_null":true,"default":""},{"name":"title","type":"text","not_null":true}],"primary_key":["key"],"indexes":[{"name":"items_title","unique":true,"columns":[{"name":"title","descending":false}],"on_conflict":"latest_wins"}]}],"migrations":[{"version":2,"statements":["UPDATE items SET position = upper(title)"]}]}"#;
/// The second schema with a nullable `rank`.
const THIRD: &str = r#"{"application":"shopping","version":3,"format":6,"tables":[{"name":"items","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"note","type":"text","not_null":false},{"name":"rank","type":"integer","not_null":false},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]},{"name":"tags","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"label","type":"text","not_null":true}],"primary_key":["key"]}]}"#;

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
async fn an_idle_old_member_adopts_a_newer_peer_generation() -> Result<(), Failure> {
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
            .all(|peer| peer["upgrade_required"] != true)
    );
    assert_eq!(generation(&b).await["current"]["schema"], 2);
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
    assert_eq!(status["application"], "current");
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

    // A member whose application declares the newer schema while it
    // reaches no issuer stays at its generation, and is told it is ahead.
    b.close().await;
    let ahead = Device::new(Database::open(open(&b_dir, SECOND, None))?)?;
    let status = generation(&ahead).await;
    assert_eq!(status["current"]["schema"], 1);
    assert_eq!(status["application"], "ahead");
    assert_eq!(ahead.titles().await, ["apples", "bread"]);
    ahead.close().await;

    // With its version 1 application it meets the issuer and adopts
    // generation 2 before it applies its frames; bread then leaves with
    // its original generation 1 bytes.
    let b = Device::new(Database::open(open(&b_dir, SCHEMA, network("b")))?)?;
    assert_eq!(generation(&b).await["application"], "current");
    let synced = b.sync().await.expect("sync");
    assert!(
        synced["peers"]
            .as_array()
            .ok_or("peer reports")?
            .iter()
            .all(|peer| peer["upgrade_required"] != true)
    );
    let status = generation(&b).await;
    assert_eq!(status["current"]["schema"], 2);
    assert_eq!(status["application"], "behind");
    b.sync().await.expect("sync");
    assert_eq!(a.titles().await, ["apples", "bread"]);
    assert_eq!(
        rows(&b, "SELECT note FROM items WHERE title='apples'").await,
        json!([[text("fresh")]])
    );
    b.close().await;
    let b = Device::new(Database::open(open(&b_dir, SECOND, network("b")))?)?;
    assert_eq!(generation(&b).await["application"], "current");
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
    let refused = Database::open(open(&a_dir, RETYPED, network("a")));
    let error = refused.err().expect("refused").to_string();
    assert!(error.contains("changed type"), "{error}");
    let a = Device::new(Database::open(open(&a_dir, SECOND, network("a")))?)?;
    assert_eq!(generation(&a).await["current"]["schema"], 2);
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_table_granted_like_another_reaches_existing_members() -> Result<(), Failure> {
    let root = common::identity::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let (a_dir, b_dir) = (root.path().join("a"), root.path().join("b"));
    let a = Device::create(&a_dir, network("a"))?;
    // Raw grants name `items` alone, as an application's device kinds do.
    let invitation = a
        .invite_as(json!({"kind": "grants", "grants": [{"scope": "items", "rights": ["read", "insert", "update", "delete"]}]}))
        .await;
    let b = Device::join(&b_dir, &invitation, Network::Owned(hub.connector("b")))?;
    introduce(&a, &b).await;
    b.close().await;
    a.close().await;

    // Declaring it on a table that already exists would widen grants.
    let existing = SCHEMA.replace(
        r#"]}]}"#,
        r#"]},{"name":"tags","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true}],"primary_key":["key"]}],"granted_like":{"items":"tags"}}"#,
    );
    let error = Database::open(open(&a_dir, &existing, network("a")))
        .err()
        .expect("refused")
        .to_string();
    assert!(error.contains("changed whom it is granted like"), "{error}");

    let a = Device::new(Database::open(open(&a_dir, GRANTED, network("a")))?)?;
    let b = Device::new(Database::open(open(&b_dir, GRANTED, network("b")))?)?;
    b.ok(json!({
        "type": "mutate",
        "handle": vec![5; 32],
        "payload": json!({"type": "create", "collection": "tags", "values": {"label": "fruit"}}).to_string().into_bytes(),
        "entity": null,
        "baseline": null,
    }))
    .await;
    b.sync().await.expect("sync");
    assert_eq!(
        rows(&a, "SELECT label FROM tags").await,
        json!([[text("fruit")]])
    );
    b.close().await;
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_required_field_migrates_on_the_issuer_and_older_devices_keep_syncing()
-> Result<(), Failure> {
    let root = common::identity::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let dirs = |name: &str| root.path().join(name);
    let a = Device::create(&dirs("a"), network("a"))?;
    let b = Device::join(
        &dirs("b"),
        &a.invite().await,
        Network::Owned(hub.connector("b")),
    )?;
    let c = Device::join(
        &dirs("c"),
        &a.invite().await,
        Network::Owned(hub.connector("c")),
    )?;
    introduce(&a, &b).await;
    introduce(&a, &c).await;
    a.add(1, "cherries").await;
    a.add(2, "apples").await;
    b.add(3, "bread").await;
    b.sync().await.expect("sync");
    c.sync().await.expect("sync");
    // Never reaches the issuer before it migrates.
    b.add(4, "late").await;
    b.close().await;
    a.close().await;

    let a = Device::new(Database::open(open(&dirs("a"), REQUIRED, network("a")))?)?;
    let status = generation(&a).await;
    assert_eq!(status["current"]["schema"], 3, "{status}");
    assert_eq!(status["application"], "current");
    let positions = json!([
        [text("apples"), text("0000")],
        [text("bread"), text("0001")],
        [text("cherries"), text("0002")]
    ]);
    assert_eq!(
        rows(&a, "SELECT title, position FROM items ORDER BY position").await,
        positions
    );

    // A device whose application stays at version 1 keeps syncing: it
    // adopts the generation, and its new item reads the default position.
    c.add(6, "stale").await;
    c.sync().await.expect("sync");
    c.sync().await.expect("sync");
    assert_eq!(generation(&c).await["application"], "behind");
    assert_eq!(a.titles().await, ["apples", "bread", "cherries", "stale"]);
    assert_eq!(
        rows(&a, "SELECT position FROM items WHERE title='stale'").await,
        json!([[text("")]])
    );
    c.close().await;

    // The member's unsynced work of generation 1 is not dropped.
    let b = Device::new(Database::open(open(&dirs("b"), REQUIRED, network("b")))?)?;
    b.sync().await.expect("sync");
    b.sync().await.expect("sync");
    assert_eq!(
        rows(
            &b,
            "SELECT title, position FROM items ORDER BY position, title"
        )
        .await,
        json!([
            [text("late"), text("")],
            [text("stale"), text("")],
            [text("apples"), text("0000")],
            [text("bread"), text("0001")],
            [text("cherries"), text("0002")]
        ])
    );
    b.ok(json!({
        "type": "mutate",
        "handle": vec![5; 32],
        "payload": json!({"type": "create", "collection": "items", "values": {"title": "dates", "completed": false, "position": "0003"}}).to_string().into_bytes(),
        "entity": null,
        "baseline": null,
    }))
    .await;
    b.sync().await.expect("sync");
    assert_eq!(
        a.titles().await,
        ["apples", "bread", "cherries", "dates", "late", "stale"]
    );
    b.close().await;
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_null_an_older_generation_writes_to_a_required_field_reads_its_default()
-> Result<(), Failure> {
    let root = common::identity::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let dirs = |name: &str| root.path().join(name);
    let a = Device::new(Database::create(
        open(&dirs("a"), NULLABLE, network("a")),
        None,
    )?)?;
    let b = Device::new(Database::join(
        open(&dirs("b"), NULLABLE, network("b")),
        &a.invite().await,
    )?)?;
    let c = Device::new(Database::join(
        open(&dirs("c"), NULLABLE, network("c")),
        &a.invite().await,
    )?)?;
    introduce(&a, &b).await;
    introduce(&a, &c).await;
    a.add(1, "apples").await;
    b.sync().await.expect("sync");
    c.sync().await.expect("sync");
    a.close().await;

    // Requiring a field that was nullable rewrites its stored NULLs as the
    // default in the switch, without a migration.
    let a = Device::new(Database::open(open(&dirs("a"), BARE, network("a")))?)?;
    assert_eq!(generation(&a).await["current"]["schema"], 2);
    assert_eq!(
        rows(&a, "SELECT title, position FROM items").await,
        json!([[text("apples"), text("")]])
    );

    // A device of generation 1 writes NULL to it before it meets the
    // issuer; every device of generation 2 reads the default.
    b.add(2, "bread").await;
    assert_eq!(
        rows(&b, "SELECT position FROM items WHERE title='bread'").await,
        json!([[{"type": "null"}]])
    );
    b.sync().await.expect("sync");
    b.sync().await.expect("sync");
    c.sync().await.expect("sync");
    for device in [&a, &b, &c] {
        assert_eq!(generation(device).await["current"]["schema"], 2);
        assert_eq!(
            rows(device, "SELECT title, position FROM items ORDER BY title").await,
            json!([[text("apples"), text("")], [text("bread"), text("")]])
        );
    }
    c.close().await;
    b.close().await;
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn requiring_a_field_rebuilds_tables_with_unique_indexes() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let a = Device::new(Database::create(open(root.path(), UNIQUE, None), None)?)?;
    a.add(1, "apples").await;
    a.add(2, "bread").await;
    a.close().await;
    let a = Device::new(Database::open(open(root.path(), UNIQUE_REQUIRED, None))?)?;
    assert_eq!(generation(&a).await["current"]["schema"], 3);
    assert_eq!(
        rows(&a, "SELECT title, position FROM items ORDER BY position").await,
        json!([
            [text("apples"), text("APPLES")],
            [text("bread"), text("BREAD")]
        ])
    );
    // The rebuilt index still refuses a local duplicate, and the table takes new rows.
    let duplicate = a
        .call(json!({
            "type": "mutate",
            "handle": vec![3; 32],
            "payload": json!({"type": "create", "collection": "items", "values": {"title": "apples", "completed": false, "position": "Z"}}).to_string().into_bytes(),
            "entity": null,
            "baseline": null,
        }))
        .await
        .expect_err("unique");
    assert!(
        duplicate["message"]
            .as_str()
            .is_some_and(|message| message.contains("UNIQUE"))
    );
    a.ok(json!({
        "type": "mutate",
        "handle": vec![4; 32],
        "payload": json!({"type": "create", "collection": "items", "values": {"title": "cherries", "completed": false, "position": "CHERRIES"}}).to_string().into_bytes(),
        "entity": null,
        "baseline": null,
    }))
    .await;
    assert_eq!(a.titles().await, ["apples", "bread", "cherries"]);
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

/// The first schema with `title` an integer: the same name, another type.
const RETYPED: &str = r#"{"application":"shopping","version":2,"format":6,"tables":[{"name":"items","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"title","type":"integer","not_null":true}],"primary_key":["key"]}]}"#;
/// The first schema with `title` renamed `label`: another field.
const RENAMED: &str = r#"{"application":"shopping","version":2,"format":6,"tables":[{"name":"items","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"label","type":"text","not_null":true}],"primary_key":["key"]}]}"#;

#[tokio::test(flavor = "multi_thread")]
async fn a_name_keeps_its_type_and_a_rename_is_another_field() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let a = Device::create(root.path(), None)?;
    a.add(1, "apples").await;
    a.close().await;
    // A rename removes the old field and adds a new one, which a required
    // field cannot be without a default.
    for (schema, refusal) in [(RETYPED, "changed type"), (RENAMED, "needs a default")] {
        let error = Database::open(open(root.path(), schema, None))
            .err()
            .expect("refused")
            .to_string();
        assert!(error.contains(refusal), "{error}");
    }
    let a = Device::new(Database::open(open(root.path(), SCHEMA, None))?)?;
    assert_eq!(generation(&a).await["current"]["schema"], 1);
    assert_eq!(a.titles().await, ["apples"]);
    a.close().await;
    Ok(())
}

const COMMANDS: &str = r#"{"application":"counter","version":1,"commands":{"format":1,"handler":1,"commands":[{"name":"Add","version":1,"handler":"counts.add.v1","arguments":[{"name":"amount","type":"integer"}],"accepted":[{"name":"value","type":"integer"}],"rejected":[],"invocation":"counts","result":"counts","resources":["counts"]}]},"format":6,"tables":[{"name":"counts","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"value","type":"integer","not_null":true}],"primary_key":["key"]}]}"#;

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

#[tokio::test(flavor = "multi_thread")]
async fn an_older_application_adopts_the_newer_generation_it_meets() -> Result<(), Failure> {
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
    a.close().await;
    // The issuer moves two generations on while the member's application
    // stays at version 1.
    Device::new(Database::open(open(&a_dir, SECOND, None))?)?
        .close()
        .await;
    let a = Device::new(Database::open(open(&a_dir, THIRD, network("a")))?)?;
    assert_eq!(generation(&a).await["current"]["schema"], 3);
    a.set(2, &apples, "rank", json!(7)).await;

    // One sync moves the member's database to generation 3 in one switch.
    b.add(3, "bread").await;
    b.sync().await.expect("sync");
    let status = generation(&b).await;
    assert_eq!(status["current"], json!({"schema": 3, "handler": 1}));
    assert_eq!(status["rebuilding"], Value::Null);
    assert_eq!(status["application"], "behind");
    // Both ways: the issuer's rank arrives, the member's bread leaves.
    b.sync().await.expect("sync");
    assert_eq!(a.titles().await, ["apples", "bread"]);
    assert_eq!(
        rows(&b, "SELECT title, rank FROM items WHERE title='apples'").await,
        json!([[text("apples"), {"type": "integer", "value": "7"}]])
    );
    b.add(4, "cheese").await;
    b.sync().await.expect("sync");
    assert_eq!(a.titles().await, ["apples", "bread", "cheese"]);
    b.close().await;

    // The version 1 application opens the generation 3 database unchanged.
    let b = Device::new(Database::open(open(&b_dir, SCHEMA, network("b")))?)?;
    let status = generation(&b).await;
    assert_eq!(status["current"]["schema"], 3);
    assert_eq!(status["application"], "behind");
    assert_eq!(b.titles().await, ["apples", "bread", "cheese"]);
    b.close().await;
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_crash_before_the_switch_resumes_at_the_newer_generation() -> Result<(), Failure> {
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
    b.add(1, "apples").await;
    b.sync().await.expect("sync");
    b.close().await;
    a.close().await;
    let a = Device::new(Database::open(open(&a_dir, SECOND, network("a")))?)?;
    // The member's file before it meets generation 2.
    let saved = root.path().join("saved");
    std::fs::create_dir(&saved)?;
    let files = ["rocky.sqlite", "rocky.sqlite-wal", "rocky.sqlite-shm"];
    for name in files {
        if b_dir.join(name).exists() {
            std::fs::copy(b_dir.join(name), saved.join(name))?;
        }
    }
    let b = Device::new(Database::open(open(&b_dir, SCHEMA, network("b")))?)?;
    b.sync().await.expect("sync");
    assert_eq!(generation(&b).await["current"]["schema"], 2);
    b.close().await;
    // A crash after the settings were saved, before the replica switched,
    // leaves the earlier file beside them.
    for name in files {
        let _ = std::fs::remove_file(b_dir.join(name));
        if saved.join(name).exists() {
            std::fs::copy(saved.join(name), b_dir.join(name))?;
        }
    }
    let b = Device::new(Database::open(open(&b_dir, SCHEMA, network("b")))?)?;
    let status = generation(&b).await;
    assert_eq!(status["current"]["schema"], 2);
    assert_eq!(status["rebuilding"], Value::Null);
    assert_eq!(status["application"], "behind");
    assert_eq!(
        rows(&b, "SELECT title, note FROM items").await,
        json!([[text("apples"), {"type": "null"}]])
    );
    b.add(2, "bread").await;
    b.sync().await.expect("sync");
    assert_eq!(a.titles().await, ["apples", "bread"]);
    b.close().await;
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_newer_horizon_is_acknowledged_after_adopting_its_generation() -> Result<(), Failure> {
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
    b.add(1, "apples").await;
    b.sync().await.expect("sync");
    a.close().await;
    let a = Device::new(Database::open(open(&a_dir, SECOND, network("a")))?)?;
    let horizon =
        |action: &'static str| json!({"type": "horizon", "collection": "items", "action": action});
    let declared = a.ok(horizon("declare")).await;
    let waiting = declared["waiting"].clone();
    assert_eq!(waiting.as_array().ok_or("waiting")?.len(), 1, "{declared}");
    // The member, at generation 1, meets the generation 2 statement.
    b.sync().await.expect("sync");
    assert_eq!(generation(&b).await["current"]["schema"], 2);
    b.sync().await.expect("sync");
    let status = a.ok(horizon("status")).await;
    assert_eq!(status["waiting"], json!([]), "{status}");
    assert!(
        status["acknowledged"]
            .as_array()
            .ok_or("acknowledged")?
            .contains(&waiting[0]),
        "{status}"
    );
    b.close().await;
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_older_application_joins_at_the_households_generation() -> Result<(), Failure> {
    let root = common::identity::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let (a_dir, b_dir) = (root.path().join("a"), root.path().join("b"));
    let a = Device::create(&a_dir, network("a"))?;
    let apples = a.add(1, "apples").await;
    a.close().await;
    let a = Device::new(Database::open(open(&a_dir, SECOND, network("a")))?)?;
    a.set(2, &apples, "note", json!("fresh")).await;
    // A device whose application declares version 1 joins: its database
    // starts at generation 2.
    let b = Device::new(Database::join(
        open(&b_dir, SCHEMA, network("b")),
        &a.invite().await,
    )?)?;
    introduce(&a, &b).await;
    b.sync().await.expect("sync");
    let status = generation(&b).await;
    assert_eq!(status["current"]["schema"], 2);
    assert_eq!(status["application"], "behind");
    assert_eq!(
        rows(&b, "SELECT title, note FROM items").await,
        json!([[text("apples"), text("fresh")]])
    );
    b.add(3, "bread").await;
    b.sync().await.expect("sync");
    assert_eq!(a.titles().await, ["apples", "bread"]);
    b.close().await;
    let b = Device::new(Database::open(open(&b_dir, SCHEMA, network("b")))?)?;
    assert_eq!(generation(&b).await["current"]["schema"], 2);
    b.close().await;
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn issuers_that_sign_one_schema_apart_end_on_one_generation() -> Result<(), Failure> {
    let root = common::identity::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let (a_dir, b_dir) = (root.path().join("a"), root.path().join("b"));
    let a = Device::create(&a_dir, network("a"))?;
    let b = Device::new(Database::join(
        open(&b_dir, SCHEMA, network("b")),
        &a.invite_as(json!({"kind": "issuer"})).await,
    )?)?;
    introduce(&a, &b).await;
    b.add(1, "apples").await;
    b.sync().await.expect("sync");
    a.close().await;
    b.close().await;
    // Each issuer signs the second schema on its own.
    let a = Device::new(Database::open(open(&a_dir, SECOND, None))?)?;
    let b = Device::new(Database::open(open(&b_dir, SECOND, None))?)?;
    let (left, right) = (generation(&a).await, generation(&b).await);
    assert_eq!(left["manifest"], right["manifest"]);
    a.close().await;
    b.close().await;
    // Both derived the first reader key of `tags` from the publisher key.
    let tags = |directory: &Path| -> Result<Value, Failure> {
        let identity: Value =
            serde_json::from_slice(&std::fs::read(directory.join("identity.json"))?)?;
        Ok(identity["keys"]["tags"].clone())
    };
    assert_eq!(tags(&a_dir)?, tags(&b_dir)?);
    assert_eq!(tags(&a_dir)?.as_array().map(Vec::len), Some(1));
    let a = Device::new(Database::open(open(&a_dir, SECOND, network("a")))?)?;
    let b = Device::new(Database::open(open(&b_dir, SECOND, network("b")))?)?;
    a.add(2, "bread").await;
    // Each issuer writes the table it added apart: both derived the same
    // first reader key, so each reads the other's rows.
    for (device, label) in [(&a, "fruit"), (&b, "dairy")] {
        device
            .ok(json!({
                "type": "mutate",
                "handle": vec![5; 32],
                "payload": json!({"type": "create", "collection": "tags", "values": {"label": label}}).to_string().into_bytes(),
                "entity": null,
                "baseline": null,
            }))
            .await;
    }
    b.sync().await.expect("sync");
    a.sync().await.expect("sync");
    for device in [&a, &b] {
        assert_eq!(
            rows(device, "SELECT label FROM tags ORDER BY label").await,
            json!([[text("dairy")], [text("fruit")]])
        );
    }
    assert_eq!(tags(&a_dir)?, tags(&b_dir)?);
    let earliest = left["baseline"]
        .as_u64()
        .min(right["baseline"].as_u64())
        .ok_or("baselines")?;
    for device in [&a, &b] {
        let status = generation(device).await;
        assert_eq!(status["manifest"], left["manifest"]);
        assert_eq!(status["current"]["schema"], 2);
        assert_eq!(status["baseline"], earliest);
        assert_eq!(device.titles().await, ["apples", "bread"]);
    }
    b.close().await;
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_older_generation_the_database_lacks_is_fetched_before_its_frames_apply()
-> Result<(), Failure> {
    let root = common::identity::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let (a_dir, b_dir, c_dir) = (
        root.path().join("a"),
        root.path().join("b"),
        root.path().join("c"),
    );
    let a = Device::create(&a_dir, network("a"))?;
    let c = Device::join(
        &c_dir,
        &a.invite().await,
        Network::Owned(hub.connector("c")),
    )?;
    introduce(&a, &c).await;
    c.sync().await.expect("sync");
    // Never leaves the generation 1 device until it meets the member.
    c.add(1, "carrots").await;
    a.close().await;
    let a = Device::new(Database::open(open(&a_dir, SECOND, network("a")))?)?;
    a.add(2, "apples").await;
    // A checkpoint sealed at generation 2.
    a.ok(
        json!({"type": "retention", "collection": "items", "policy": {
        "required": [], "behind": [], "copies": 1, "window": 256
    }, "maintain": true}),
    )
    .await;
    // The member joins at generation 2; its file then drops generation
    // 1's manifest, which nothing it holds names.
    let b = Device::new(Database::join(
        open(&b_dir, SECOND, network("b")),
        &a.invite().await,
    )?)?;
    introduce(&a, &b).await;
    b.sync().await.expect("sync");
    b.close().await;
    let file = rusqlite::Connection::open(b_dir.join("rocky.sqlite"))?;
    assert_eq!(
        file.execute("DELETE FROM secure_generations WHERE schema=1", [])?,
        1
    );
    drop(file);
    let b = Device::new(Database::open(open(&b_dir, SECOND, network("b")))?)?;
    assert_eq!(generation(&b).await["previous"], json!([]));
    // The generation 1 frame reaches the issuer, which relays it to the
    // member: the member fetches its manifest from the issuer first.
    c.sync().await.expect("sync");
    c.sync().await.expect("sync");
    assert!(a.titles().await.contains(&"carrots".to_string()));
    b.sync().await.expect("sync");
    b.sync().await.expect("sync");
    let status = generation(&b).await;
    assert_eq!(status["current"]["schema"], 2);
    assert_eq!(status["previous"], json!([{"schema": 1, "handler": 1}]));
    let activity = b.ok(json!({"type": "activity"})).await;
    assert!(
        activity["events"]
            .as_array()
            .ok_or("events")?
            .iter()
            .any(|event| event["kind"] == "generation_learned")
    );
    assert!(b.titles().await.contains(&"carrots".to_string()));
    c.close().await;
    b.close().await;
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_manifest_nothing_retained_names_is_dropped_and_fetched_again() -> Result<(), Failure> {
    let root = common::identity::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let dirs = |name: &str| root.path().join(name);
    let a = Device::create(&dirs("a"), network("a"))?;
    // A second issuer holds a copy of every frame and acknowledges the
    // horizons, so both prune.
    let b = Device::join(
        &dirs("b"),
        &a.invite_as(json!({"kind": "issuer"})).await,
        Network::Owned(hub.connector("b")),
    )?;
    introduce(&a, &b).await;
    let c = Device::join(
        &dirs("c"),
        &a.invite().await,
        Network::Owned(hub.connector("c")),
    )?;
    introduce(&a, &c).await;
    a.add(1, "apples").await;
    b.sync().await.expect("sync");
    c.sync().await.expect("sync");
    // A device that then stays away at generation 1.
    c.close().await;
    a.close().await;

    let a = Device::new(Database::open(open(&dirs("a"), SECOND, network("a")))?)?;
    b.sync().await.expect("sync");
    for device in [&a, &b] {
        assert_eq!(
            generation(device).await["previous"],
            json!([{"schema": 1, "handler": 1}])
        );
    }
    // Every retained frame of generation 1 is finalized and pruned.
    let participant = b.node().await;
    for collection in ["items", "issuer_registry"] {
        let retention = json!({"type": "retention", "collection": collection,
            "policy": {"required": [], "behind": [], "copies": 1, "window": 0}});
        a.ok(retention.clone()).await;
        b.ok(retention).await;
        // The registry's horizon names no participant: `b` follows it.
        let participants = if collection == "items" {
            vec![participant.clone()]
        } else {
            Vec::new()
        };
        let horizon = |action: &str| json!({"type": "horizon", "collection": collection, "action": action, "participants": participants});
        a.ok(horizon("declare")).await;
        b.sync().await.expect("sync");
        b.sync().await.expect("sync");
        let finalized = a.ok(horizon("finalize")).await;
        assert_eq!(finalized["finalized"], true, "{collection}: {finalized}");
        b.sync().await.expect("sync");
    }
    // Nothing retained names generation 1: its manifest is gone from both
    // devices, replica and saved lineage, and the transition stays.
    for (device, name) in [(&a, "a"), (&b, "b")] {
        assert_eq!(generation(device).await["previous"], json!([]), "{name}");
        let identity: Value =
            serde_json::from_slice(&std::fs::read(dirs(name).join("identity.json"))?)?;
        assert_eq!(identity["lineage"][0].get("manifest"), None, "{identity}");
        assert!(identity["lineage"][0]["transition"].is_string());
    }
    a.close().await;

    // `c` writes under generation 1 while still away, then returns and
    // reaches `b`: its frame makes `b` fetch the manifest again. The row
    // itself stays hidden, as for any device that missed a finalized
    // horizon; the replica's own tests apply such a frame.
    let c = Device::new(Database::open(open(&dirs("c"), SCHEMA, None))?)?;
    c.add(2, "late").await;
    c.close().await;
    let c = Device::new(Database::open(open(&dirs("c"), SCHEMA, network("c")))?)?;
    introduce(&b, &c).await;
    for _ in 0..3 {
        let _ = c.sync().await;
        let _ = b.sync().await;
    }
    assert_eq!(
        generation(&b).await["previous"],
        json!([{"schema": 1, "handler": 1}])
    );
    let activity = b.ok(json!({"type": "activity"})).await;
    let events = activity["events"].as_array().ok_or("events")?;
    assert!(
        events
            .iter()
            .any(|event| event["kind"] == "generation_dropped")
    );
    assert!(
        events
            .iter()
            .any(|event| event["kind"] == "generation_learned")
    );
    let identity: Value = serde_json::from_slice(&std::fs::read(dirs("b").join("identity.json"))?)?;
    assert!(identity["lineage"][0]["manifest"].is_string(), "{identity}");
    c.close().await;
    b.close().await;
    Ok(())
}
