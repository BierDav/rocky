//! Schema migrations of any size: the issuer signs the generation, runs its
//! migrations in one local transaction and publishes the result as many
//! bundles. See `docs/runtime/generations.md#migrations`.
mod common;
use common::{Device, Failure, introduce};
use rocky_sdk::{Database, Network, Open, transport::memory::Hub};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

/// Requires `position`, which the migration fills from the title, and adds
/// `tags`, keyed by name, with one row per item. Running it twice would
/// double each position.
const POSITIONED: &str = r#"{"application":"shopping","version":2,"format":6,"tables":[
 {"name":"items","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"position","type":"text","not_null":true,"default":""},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]},
 {"name":"tags","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"label","type":"text","not_null":true}],"primary_key":["key"]}],
 "migrations":[{"version":2,"statements":["UPDATE items SET position = position || title",
  {"insert":"tags","select":"SELECT 'tag-' || key AS key, title AS label FROM items"}]}]}"#;
/// `POSITIONED` whose migration asks for an id, which no migration may.
const ASKS_FOR_IDS: &str = r#"{"application":"shopping","version":2,"format":6,"tables":[
 {"name":"items","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"position","type":"text","not_null":true,"default":""},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]},
 {"name":"tags","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"label","type":"text","not_null":true}],"primary_key":["key"]}],
 "migrations":[{"version":2,"statements":["UPDATE items SET position = rocky_next_id('items')"]}]}"#;
/// The fixed release after `ASKS_FOR_IDS`: the same tables, the version 2
/// migration of `POSITIONED`.
const FIXED: &str = r#"{"application":"shopping","version":3,"format":6,"tables":[
 {"name":"items","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"position","type":"text","not_null":true,"default":""},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]},
 {"name":"tags","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"label","type":"text","not_null":true}],"primary_key":["key"]}],
 "migrations":[{"version":2,"statements":["UPDATE items SET position = position || title",
  {"insert":"tags","select":"SELECT 'tag-' || key AS key, title AS label FROM items"}]}]}"#;

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
const POSITIONS: &str = "SELECT title, position FROM items ORDER BY title";
const TAGS: &str = "SELECT key, label FROM tags ORDER BY key";

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_migration_runs_again_and_a_committed_one_never() -> Result<(), Failure> {
    let root = common::identity::small_directory()?;
    let a = Device::create(root.path(), None)?;
    a.add(1, "apples").await;
    a.add(2, "bread").await;
    a.close().await;

    // The issuer signs first; the migration then fails before it commits.
    let refused = Database::open(open(root.path(), ASKS_FOR_IDS, None))
        .err()
        .ok_or("a migration asking for ids opened")?;
    assert!(
        refused.to_string().contains(
            "migration to version 2 failed at statement 1: SQL session: statement is not authorized"
        ),
        "{refused}"
    );

    // The fixed release runs the migration again, once.
    let a = Device::new(Database::open(open(root.path(), FIXED, None))?)?;
    let status = generation(&a).await;
    assert_eq!(status["current"]["schema"], 2, "{status}");
    assert_eq!(
        status["migration"],
        json!({"state": "publishing", "bundles": 1, "published": 0})
    );
    let positions = json!([
        [text("apples"), text("apples")],
        [text("bread"), text("bread")]
    ]);
    let tags = rows(&a, TAGS).await;
    assert_eq!(rows(&a, POSITIONS).await, positions);
    assert_eq!(tags.as_array().map(Vec::len), Some(2));
    a.close().await;

    // A crash after the migration committed, before its intent was removed,
    // repeats nothing.
    std::fs::write(root.path().join("migration.json"), br#"{"from":1}"#)?;
    let a = Device::new(Database::open(open(root.path(), FIXED, None))?)?;
    assert_eq!(rows(&a, POSITIONS).await, positions);
    assert_eq!(rows(&a, TAGS).await, tags);
    assert_eq!(
        generation(&a).await["migration"],
        json!({"state": "publishing", "bundles": 1, "published": 0})
    );
    assert!(!root.path().join("migration.json").exists());
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn issuers_that_cannot_reach_each_other_both_migrate_and_converge() -> Result<(), Failure> {
    let root = common::identity::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let (a_dir, b_dir) = (root.path().join("a"), root.path().join("b"));
    let a = Device::create(&a_dir, network("a"))?;
    let b = Device::new(Database::join(
        open(&b_dir, common::SCHEMA, network("b")),
        &a.invite_as(json!({"kind": "issuer"})).await,
    )?)?;
    introduce(&a, &b).await;
    a.add(1, "apples").await;
    b.add(2, "bread").await;
    b.sync().await.expect("sync");
    a.close().await;
    b.close().await;

    // Apart, each signs the generation and migrates what it holds.
    for directory in [&a_dir, &b_dir] {
        let device = Device::new(Database::open(open(directory, POSITIONED, None))?)?;
        assert_eq!(generation(&device).await["current"]["schema"], 2);
        device.close().await;
    }
    let a = Device::new(Database::open(open(&a_dir, POSITIONED, network("a")))?)?;
    let b = Device::new(Database::open(open(&b_dir, POSITIONED, network("b")))?)?;
    b.sync().await.expect("sync");
    a.sync().await.expect("sync");
    b.sync().await.expect("sync");
    // Updates meet by latest wins; inserted rows keyed by name merge, so
    // none is there twice.
    for device in [&a, &b] {
        assert_eq!(
            rows(device, POSITIONS).await,
            json!([
                [text("apples"), text("apples")],
                [text("bread"), text("bread")]
            ])
        );
        let tags = rows(device, TAGS).await;
        assert_eq!(tags.as_array().map(Vec::len), Some(2), "{tags}");
        assert_eq!(generation(device).await["migration"], Value::Null);
    }
    assert_eq!(rows(&a, TAGS).await, rows(&b, TAGS).await);
    b.close().await;
    a.close().await;
    Ok(())
}

const LISTS: &str = r#"{"format":6,"application":"lists","version":1,"tables":[
 {"name":"lists","class":"mergeable","columns":[{"name":"title","type":"text","not_null":true}],
  "primary_key":["id"],"partition_by":["id"],"partition_group":"list"}]}"#;
/// Requires `position` of every list, which a migration fills.
const POSITIONED_LISTS: &str = r#"{"format":6,"application":"lists","version":2,"tables":[
 {"name":"lists","class":"mergeable","columns":[{"name":"title","type":"text","not_null":true},{"name":"position","type":"text","not_null":true,"default":""}],
  "primary_key":["id"],"partition_by":["id"],"partition_group":"list"}],
 "migrations":[{"version":2,"statements":["UPDATE lists SET position = title"]}]}"#;

/// Creates a list in its own partition; returns its id.
async fn list(device: &Device, title: &str) -> Result<String, Failure> {
    device.ok(json!({"type": "authors"})).await;
    device
        .ok(json!({"type": "partition_next", "table": "lists"}))
        .await;
    device.ok(json!({"type": "sql", "sql": "BEGIN"})).await;
    let id = device
        .ok(json!({"type": "next_id", "table": "lists"}))
        .await["id"]
        .clone();
    device
        .ok(json!({"type": "sql", "sql": format!("INSERT INTO lists(id,title) VALUES(?,'{title}')"), "values": [{"type": "integer", "value": id}]}))
        .await;
    device.ok(json!({"type": "sql", "sql": "COMMIT"})).await;
    Ok(id.as_str().ok_or("id")?.to_owned())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_issuer_without_a_partitions_key_keeps_the_households_generation() -> Result<(), Failure>
{
    let root = common::identity::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let (a_dir, b_dir) = (root.path().join("a"), root.path().join("b"));
    let a = Device::new(Database::create(open(&a_dir, LISTS, network("a")), None)?)?;
    let b = Device::new(Database::join(
        open(&b_dir, LISTS, network("b")),
        &a.invite_as(json!({"kind": "issuer"})).await,
    )?)?;
    introduce(&a, &b).await;
    list(&a, "From a").await?;
    let theirs = list(&b, "From b").await?;
    // `a` learns of `b`'s list before the list's key reached it.
    a.ok(json!({"type": "partition_register", "group": "list", "values": [{"type": "integer", "value": theirs}]}))
        .await;
    a.close().await;

    // Holding no key of that list, `a` does not sign.
    let a = Device::new(Database::open(open(
        &a_dir,
        POSITIONED_LISTS,
        network("a"),
    ))?)?;
    let status = generation(&a).await;
    assert_eq!(status["current"]["schema"], 1, "{status}");
    assert_eq!(status["application"], "ahead");
    assert_eq!(
        status["migration"],
        json!({"state": "full_access_required"})
    );
    let query = a
        .call(json!({"type": "read", "query": {"sql": "SELECT position FROM lists WHERE row_kind = 0", "values": [], "max_rows": 8}}))
        .await
        .err()
        .ok_or("a query on the new field answered")?;
    assert!(query.to_string().contains("no such column"), "{query}");

    // Issuers pass each other their group keys: once `a` holds it, it
    // signs and migrates.
    b.sync().await.expect("sync");
    a.sync().await.expect("sync");
    a.close().await;
    let a = Device::new(Database::open(open(
        &a_dir,
        POSITIONED_LISTS,
        network("a"),
    ))?)?;
    assert_eq!(generation(&a).await["current"]["schema"], 2);
    assert_eq!(
        rows(
            &a,
            "SELECT title, position FROM lists WHERE row_kind = 0 ORDER BY title"
        )
        .await,
        json!([
            [text("From a"), text("From a")],
            [text("From b"), text("From b")]
        ])
    );
    // `b` adopts the generation, then receives the migrated rows.
    b.sync().await.expect("sync");
    b.sync().await.expect("sync");
    assert_eq!(
        rows(
            &b,
            "SELECT title, position FROM lists WHERE row_kind = 0 ORDER BY title"
        )
        .await,
        json!([
            [text("From a"), text("From a")],
            [text("From b"), text("From b")]
        ])
    );
    b.close().await;
    a.close().await;
    Ok(())
}

/// Rows of the full-size migration.
const LARGE: usize = 100_000;
/// Adds a nullable note and seeds `LARGE` items with a migration.
const SEEDED: &str = r#"{"application":"shopping","version":2,"format":6,"tables":[
 {"name":"items","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"note","type":"text"},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]}],
 "migrations":[{"version":2,"statements":[{"insert":"items","select":"WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 100000) SELECT printf('k%06d', i) AS key, 0 AS completed, printf('item %06d', i) AS title FROM n"}]}]}"#;
/// Requires `position` of every item, filled from its title.
const FILLED: &str = r#"{"application":"shopping","version":3,"format":6,"tables":[
 {"name":"items","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"note","type":"text"},{"name":"position","type":"text","not_null":true,"default":""},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]}],
 "migrations":[{"version":2,"statements":[{"insert":"items","select":"WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 100000) SELECT printf('k%06d', i) AS key, 0 AS completed, printf('item %06d', i) AS title FROM n"}]},
  {"version":3,"statements":["UPDATE items SET position = title"]}]}"#;
const FILLED_COUNT: &str =
    "SELECT count(*), count(CASE WHEN position = title THEN 1 END) FROM items";

/// The e2e tier runs it: measured with its evidence in
/// `docs/features_09/07-migrations-at-any-size.md`.
#[tokio::test(flavor = "multi_thread")]
async fn a_migration_of_100000_rows_converges_on_every_member() -> Result<(), Failure> {
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
    b.sync().await.expect("sync");
    a.close().await;

    let size =
        |directory: &Path| std::fs::metadata(directory.join("rocky.sqlite")).map(|m| m.len());
    let before = size(&a_dir)?;
    let clock = std::time::Instant::now();
    let (seed, fill) = (
        SEEDED.replace("100000", &LARGE.to_string()),
        FILLED.replace("100000", &LARGE.to_string()),
    );
    let a = Device::new(Database::open(open(&a_dir, &seed, network("a")))?)?;
    let seeded = clock.elapsed();
    a.close().await;
    let between = size(&a_dir)?;
    let clock = std::time::Instant::now();
    let a = Device::new(Database::open(open(&a_dir, &fill, network("a")))?)?;
    let filled = clock.elapsed();
    let status = generation(&a).await;
    assert_eq!(status["current"]["schema"], 3, "{status}");
    let bundles = LARGE.div_ceil(rocky_secure::MAX_BUNDLE_ROWS) as u64;
    assert_eq!(
        status["migration"],
        json!({"state": "publishing", "bundles": 2 * bundles, "published": 0})
    );
    let count = |n: usize| json!([[{"type": "integer", "value": n.to_string()}, {"type": "integer", "value": n.to_string()}]]);
    assert_eq!(rows(&a, FILLED_COUNT).await, count(LARGE));

    // The member, whose application stays at version 1, pulls until it
    // holds every row; each sync resumes where the last stopped.
    let clock = std::time::Instant::now();
    let mut syncs = 0;
    while rows(&b, "SELECT count(*) FROM items").await
        != json!([[{"type": "integer", "value": LARGE.to_string()}]])
        || b.call(
            json!({"type": "read", "query": {"sql": FILLED_COUNT, "values": [], "max_rows": 1}}),
        )
        .await
        .ok()
        .map(|page| page["rows"].clone())
            != Some(count(LARGE))
    {
        syncs += 1;
        assert!(syncs <= 64, "the member did not converge");
        let _ = b.sync().await;
    }
    let pulled = clock.elapsed();
    a.sync().await.expect("sync");
    assert_eq!(generation(&a).await["migration"], Value::Null);
    let fingerprint =
        "SELECT count(*), min(position), max(position), sum(length(position)) FROM items";
    assert_eq!(rows(&a, fingerprint).await, rows(&b, fingerprint).await);
    eprintln!(
        "MEASURED rows={LARGE} seed_open={seeded:?} fill_open={filled:?} file_before={before} file_seeded={between} file_filled={} member_syncs={syncs} member_pull={pulled:?} member_file={}",
        size(&a_dir)?,
        size(&b_dir)?,
    );
    b.close().await;
    a.close().await;
    Ok(())
}
