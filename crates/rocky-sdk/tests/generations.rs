//! Schema generations: issuers sign them and members fetch them while
//! older devices keep their work. See
//! `docs/runtime/generations.md`.
mod common;
use common::{Device, Failure, SCHEMA, introduce};
use rocky_sdk::{Database, Network, Open, transport::memory::Hub};
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
        identity: None,
        directory,
        schema,
        secrets: None,
        network,
        coordination: None,
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
async fn an_idle_old_member_keeps_its_generation_when_it_meets_a_newer_peer() -> Result<(), Failure>
{
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
    assert_eq!(generation(&b).await["current"]["schema"], 1);
    assert!(report.get("upgrade").is_none());
    let activity = b.ok(json!({"type": "activity"})).await;
    assert!(
        activity["events"]
            .as_array()
            .ok_or("events")?
            .iter()
            .all(|event| event["kind"] != "generation_adopted")
    );
    b.close().await;
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_explicit_schema_open_preserves_pending_work_locally() -> Result<(), Failure> {
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
    assert_eq!(status["current"], json!({"schema": 2}));
    assert_eq!(status["application"], "current");
    assert_eq!(status["previous"], json!([{"schema": 1}]));
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

    // An explicit target schema open verifies and installs generation 2.
    // Bread retains its original tip and stays local after the switch.
    let b = Device::new(Database::open(open(&b_dir, SECOND, network("b")))?)?;
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
    assert_eq!(status["application"], "current");
    b.sync().await.expect("sync");
    assert_eq!(a.titles().await, ["apples"]);
    assert_eq!(b.titles().await, ["apples", "bread"]);
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
    // Adopting renews nothing; the next sync asks the issuer once for the
    // grant and key of the table that follows `items`.
    let before = chain(&b_dir)?;
    b.sync().await.expect("sync");
    assert_ne!(chain(&b_dir)?, before);
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
async fn a_required_field_migrates_and_explicitly_upgraded_devices_keep_syncing()
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
    b.close().await;
    a.close().await;

    let a = Device::new(Database::open(open(&dirs("a"), REQUIRED, network("a")))?)?;
    // Signed first, then migrated in that generation: one step.
    let status = generation(&a).await;
    assert_eq!(status["current"]["schema"], 2, "{status}");
    assert_eq!(status["application"], "current");
    assert_eq!(
        status["migration"],
        json!({"state": "publishing", "bundles": 1, "published": 0})
    );
    let positions = json!([
        [text("apples"), text("0000")],
        [text("bread"), text("0001")],
        [text("cherries"), text("0002")]
    ]);
    assert_eq!(
        rows(&a, "SELECT title, position FROM items ORDER BY position").await,
        positions
    );

    // Explicitly install the signed generation before exchanging its frames.
    c.close().await;
    let c = Device::new(Database::open(open(&dirs("c"), REQUIRED, network("c")))?)?;
    c.add(6, "stale").await;
    c.sync().await.expect("sync");
    c.sync().await.expect("sync");
    assert_eq!(generation(&c).await["application"], "current");
    // A peer holds the migration's bundle: nothing is left to publish.
    assert_eq!(generation(&a).await["migration"], Value::Null);
    assert_eq!(a.titles().await, ["apples", "bread", "cherries", "stale"]);
    assert_eq!(
        rows(&a, "SELECT position FROM items WHERE title='stale'").await,
        json!([[text("")]])
    );
    c.close().await;

    // A member explicitly switches before writing to the current tip.
    let b = Device::new(Database::open(open(&dirs("b"), REQUIRED, network("b")))?)?;
    b.add(4, "late").await;
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
async fn requiring_a_nullable_field_converts_retained_nulls_on_every_explicit_upgrade()
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
    b.add(2, "bread").await;
    b.sync().await.expect("sync");
    c.sync().await.expect("sync");
    a.close().await;

    // Requiring a field that was nullable rewrites its stored NULLs as the
    // default in the switch, without a migration.
    let a = Device::new(Database::open(open(&dirs("a"), BARE, network("a")))?)?;
    assert_eq!(generation(&a).await["current"]["schema"], 2);
    assert_eq!(
        rows(&a, "SELECT title, position FROM items ORDER BY title").await,
        json!([[text("apples"), text("")], [text("bread"), text("")]])
    );

    // The retained generation 1 NULL reads as the default after each
    // explicit schema switch.
    assert_eq!(
        rows(&b, "SELECT position FROM items WHERE title='bread'").await,
        json!([[{"type": "null"}]])
    );
    b.close().await;
    c.close().await;
    let b = Device::new(Database::open(open(&dirs("b"), BARE, network("b")))?)?;
    let c = Device::new(Database::open(open(&dirs("c"), BARE, network("c")))?)?;
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
    assert_eq!(generation(&a).await["current"]["schema"], 2);
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
    assert_eq!(status["current"], json!({"schema": 1}));
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

#[tokio::test(flavor = "multi_thread")]
async fn an_older_application_keeps_its_workspace_when_it_meets_a_newer_generation()
-> Result<(), Failure> {
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

    // Peer discovery must leave the local schema and unsent rows alone.
    b.add(3, "bread").await;
    let _ = b.sync().await;
    let status = generation(&b).await;
    assert_eq!(status["current"], json!({"schema": 1}));
    assert_eq!(status["rebuilding"], Value::Null);
    assert_eq!(status["application"], "current");
    assert_eq!(b.titles().await, ["apples", "bread"]);
    assert_eq!(a.titles().await, ["apples"]);
    let error = b.call(json!({"type": "read", "query": {"sql": "SELECT rank FROM items", "values": [], "max_rows": 16}})).await.expect_err("peer schema was not installed");
    assert!(
        error["message"]
            .as_str()
            .is_some_and(|message| message.contains("no such column")),
        "{error}"
    );
    b.close().await;
    let b = Device::new(Database::open(open(&b_dir, SCHEMA, network("b")))?)?;
    assert_eq!(generation(&b).await["current"]["schema"], 1);
    assert_eq!(b.titles().await, ["apples", "bread"]);
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
    let b = Device::new(Database::open(open(&b_dir, SECOND, network("b")))?)?;
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

/// The credential chain a device's identity holds.
fn chain(directory: &Path) -> Result<Value, Failure> {
    let identity: Value = serde_json::from_slice(&std::fs::read(directory.join("identity.json"))?)?;
    Ok(identity["credentials"].clone())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_explicit_schema_open_fetches_from_a_member_with_no_issuer_reachable()
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
    let b = Device::join(
        &b_dir,
        &a.invite().await,
        Network::Owned(hub.connector("b")),
    )?;
    let c = Device::join(
        &c_dir,
        &a.invite().await,
        Network::Owned(hub.connector("c")),
    )?;
    introduce(&a, &b).await;
    introduce(&a, &c).await;
    introduce(&b, &c).await;
    let apples = c.add(1, "apples").await;
    c.sync().await.expect("sync");
    b.sync().await.expect("sync");
    a.close().await;
    // The issuer signs generation 2 and meets only `b` before it leaves.
    let a = Device::new(Database::open(open(&a_dir, SECOND, network("a")))?)?;
    a.set(2, &apples, "note", json!("fresh")).await;
    b.close().await;
    let b = Device::new(Database::open(open(&b_dir, SECOND, network("b")))?)?;
    b.sync().await.expect("sync");
    assert_eq!(generation(&b).await["current"]["schema"], 2);
    b.sync().await.expect("sync");
    assert_eq!(
        rows(&b, "SELECT note FROM items WHERE title='apples'").await,
        json!([[text("fresh")]])
    );
    a.close().await;

    // A peer alone never changes c's installed schema.
    let before = chain(&c_dir)?;
    let _ = c.sync().await;
    assert_eq!(generation(&c).await["current"]["schema"], 1);
    assert_eq!(chain(&c_dir)?, before);
    c.close().await;
    // An explicit schema open can fetch its signed definition from b.
    let c = Device::new(Database::open(open(&c_dir, SECOND, network("c")))?)?;
    assert_eq!(generation(&c).await["application"], "current");
    let bread = c.add(3, "bread").await;
    let _ = c.sync().await;
    assert_eq!(b.titles().await, ["apples", "bread"]);
    c.set(4, &bread, "note", json!("rye")).await;
    let _ = c.sync().await;
    assert_eq!(
        rows(&b, "SELECT note FROM items WHERE title='bread'").await,
        json!([[text("rye")]])
    );
    assert_eq!(chain(&c_dir)?, before);
    c.close().await;
    b.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_invitation_created_before_a_schema_change_enrolls_a_device_after_it()
-> Result<(), Failure> {
    let root = common::identity::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let (a_dir, b_dir) = (root.path().join("a"), root.path().join("b"));
    let a = Device::create(&a_dir, network("a"))?;
    let invitation = a.invite().await;
    a.add(1, "apples").await;
    a.close().await;
    let a = Device::new(Database::open(open(&a_dir, SECOND, network("a")))?)?;
    assert_eq!(generation(&a).await["current"]["schema"], 2);
    // The invitation names tables, not a generation: it enrolls into the
    // one the issuer runs now.
    let b = Device::new(Database::join(
        open(&b_dir, SECOND, network("b")),
        &invitation,
    )?)?;
    introduce(&a, &b).await;
    b.sync().await.expect("sync");
    let status = generation(&b).await;
    assert_eq!(status["current"]["schema"], 2);
    assert_eq!(status["application"], "current");
    let bread = b.add(2, "bread").await;
    b.set(3, &bread, "note", json!("rye")).await;
    b.sync().await.expect("sync");
    assert_eq!(a.titles().await, ["apples", "bread"]);
    assert_eq!(
        rows(&a, "SELECT note FROM items WHERE title='bread'").await,
        json!([[text("rye")]])
    );
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
async fn an_unknown_older_generation_is_not_fetched_on_peer_discovery() -> Result<(), Failure> {
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
    // Neither peer discovery nor a refused frame restores a dropped schema.
    let _ = c.sync().await;
    let _ = b.sync().await;
    let status = generation(&b).await;
    assert_eq!(status["current"]["schema"], 2);
    assert_eq!(status["previous"], json!([]));
    let activity = b.ok(json!({"type": "activity"})).await;
    assert!(
        activity["events"]
            .as_array()
            .ok_or("events")?
            .iter()
            .all(|event| event["kind"] != "generation_learned")
    );
    assert_eq!(b.titles().await, ["apples"]);
    c.close().await;
    b.close().await;
    a.close().await;
    Ok(())
}
