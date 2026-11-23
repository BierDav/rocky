use crate::common;
mod prepared_pull;
mod replayed;
use crate::support::snapshot as snap;
use common::{Device, Failure, introduce};
use rocky_sdk::{Database, Network, Open, transport::memory::Hub};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

const POSITIONED: &str = r#"{"application":"shopping","version":2,"format":7,"tables":[
 {"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"position","type":"text","not_null":true,"default":""},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]},
 {"name":"tags","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"label","type":"text","not_null":true}],"primary_key":["key"]}],
 "migrations":[{"version":2,"statements":["UPDATE items SET position = position || title",
  {"insert":"tags","select":"SELECT 'tag-' || key AS key, title AS label FROM items"}]}]}"#;
const ASKS_FOR_IDS: &str = r#"{"application":"shopping","version":2,"format":7,"tables":[
 {"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"position","type":"text","not_null":true,"default":""},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]},
 {"name":"tags","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"label","type":"text","not_null":true}],"primary_key":["key"]}],
 "migrations":[{"version":2,"statements":["UPDATE items SET position = rocky_next_id('items')"]}]}"#;
const FIXED: &str = r#"{"application":"shopping","version":3,"format":7,"tables":[
 {"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"position","type":"text","not_null":true,"default":""},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]},
 {"name":"tags","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"label","type":"text","not_null":true}],"primary_key":["key"]}],
 "migrations":[{"version":2,"statements":["UPDATE items SET position = position || title",
  {"insert":"tags","select":"SELECT 'tag-' || key AS key, title AS label FROM items"}]}]}"#;

fn reopen(options: Open<'_>) -> Result<Device, Failure> {
    let signer = options
        .identity
        .clone()
        .unwrap_or_else(|| common::external_identity(options.directory));
    Device::new(Database::open(options)?).map(|device| device.with_identity(signer))
}

fn open<'a>(directory: &'a Path, schema: &'a str, network: Option<Network>) -> Open<'a> {
    Open {
        identity: Some(common::external_identity(directory)),
        directory,
        schema: crate::support::snapshot::schema(schema),
        secrets: None,
        network,
    }
}
async fn sync(device: &Device) {
    let report = device.sync().await.expect("sync");
    if std::env::var_os("ROCKY_TRACE_MIGRATION").is_some() {
        eprintln!("migration report: {report}");
    }
    for peer in report["peers"].as_array().into_iter().flatten() {
        for attempt in peer["attempts"].as_array().into_iter().flatten() {
            if !attempt["failure"].is_null() {
                eprintln!("migration synchronization: {}", attempt["failure"]);
            }
        }
    }
}
async fn generation(device: &Device) -> Value {
    snap::ok(device.session.generation()).await
}
async fn rows(device: &Device, sql: &str) -> Value {
    snap::ok(snap::read(&device.database, sql, Vec::new())).await["rows"].clone()
}
fn text(value: &str) -> rocky_sdk::SqlValue {
    rocky_sdk::SqlValue::Text(snap::text(&(value)))
}
const POSITIONS: &str = "SELECT title, position FROM items ORDER BY title";
const TAGS: &str = "SELECT key, label FROM tags ORDER BY key";

const DUPLICATES: &str = r#"{"application":"shopping","version":2,"format":7,"tables":[
 {"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"position","type":"text","not_null":true,"default":""},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]},
 {"name":"tags","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"label","type":"text","not_null":true}],"primary_key":["key"]}],
 "migrations":[{"version":2,"statements":["UPDATE items SET position = position || title",
  {"insert":"tags","select":"SELECT 'same' AS key, title AS label FROM items"}]}]}"#;

async fn graph(device: &Device) -> (Value, usize) {
    let tip = snap::ok(device.session.tip_status()).await["tip"].clone();
    let seals = snap::ok(device.session.inspect_tips()).await["seals"]
        .as_array()
        .map_or(0, Vec::len);
    (tip, seals)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_migration_runs_again_and_a_committed_one_never() -> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let a = Device::create(root.path(), None)?;
    a.add(1, "apples").await;
    a.add(2, "bread").await;
    let before = graph(&a).await;
    a.close().await;

    let refused = Database::open(open(root.path(), ASKS_FOR_IDS, None))
        .err()
        .ok_or("a migration asking for ids opened")?;
    assert!(
        refused.to_string().contains("rocky_next_id"),
        "a migration asking for an id is refused before anything is signed: {refused}"
    );
    let failed = Database::open(open(root.path(), DUPLICATES, None))
        .err()
        .ok_or("a failing migration opened")?;
    assert!(
        failed
            .to_string()
            .contains("migration to version 2 failed at statement 2"),
        "{failed}"
    );
    let a = reopen(open(root.path(), common::SCHEMA, None))?;
    let status = generation(&a).await;
    assert_eq!(status["current"]["schema"], 1, "{status}");
    assert_eq!(status["application"], "current");
    assert_eq!(
        graph(&a).await,
        before,
        "a failed migration withdraws its private seal and opening"
    );
    assert_eq!(
        a.titles().await,
        ["apples", "bread"],
        "after a failed migration the workspace is the source tip's"
    );
    a.close().await;

    let a = reopen(open(root.path(), FIXED, None))?;
    let status = generation(&a).await;
    assert_eq!(status["current"]["schema"], 3, "{status}");
    assert_eq!(
        status["migration"],
        json!({"state": "publishing", "bundles": 1, "published": 0})
    );
    let positions = json!([
        [snap::cell(text("apples")), snap::cell(text("apples"))],
        [snap::cell(text("bread")), snap::cell(text("bread"))]
    ]);
    let tags = rows(&a, TAGS).await;
    assert_eq!(
        rows(&a, POSITIONS).await,
        positions,
        "the fixed release runs the migration again"
    );
    assert_eq!(tags.as_array().map(Vec::len), Some(2));
    a.close().await;

    let a = reopen(open(root.path(), FIXED, None))?;
    assert_eq!(rows(&a, POSITIONS).await, positions);
    assert_eq!(
        rows(&a, TAGS).await,
        tags,
        "the publication is recorded with its rows, so reopening repeats nothing"
    );
    assert_eq!(
        generation(&a).await["migration"],
        json!({"state": "publishing", "bundles": 1, "published": 0})
    );
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn issuers_that_cannot_reach_each_other_both_migrate_and_converge() -> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let (a_dir, b_dir) = (root.path().join("a"), root.path().join("b"));
    let a = Device::create(&a_dir, network("a"))?;
    let b_opened = Database::join(
        open(&b_dir, common::SCHEMA, network("b")),
        &a.invite_as(rocky_sdk::Access::Full).await,
    )?;
    let b_identity: Arc<dyn rocky_sdk::IdentityProvider> =
        Arc::new(rocky_sdk::IdentityKey::from_phrase(
            b_opened.identity.as_deref().ok_or("linkable identity")?,
        )?);
    let b = Device::new(b_opened)?;
    a.admit(&b).await;
    introduce(&a, &b).await;
    a.add(1, "apples").await;
    b.add(2, "bread").await;
    sync(&b).await;
    a.close().await;
    b.close().await;

    for (directory, id) in [(&a_dir, None), (&b_dir, Some(b_identity.clone()))] {
        let mut opts = open(directory, POSITIONED, None);
        if let Some(id) = id {
            opts.identity = Some(id);
        }
        let device = reopen(opts)?;
        assert_eq!(
            generation(&device).await["current"]["schema"],
            2,
            "apart, each issuer signs the generation and migrates what it holds"
        );
        device.close().await;
    }
    let a = reopen(open(&a_dir, POSITIONED, network("a")))?;
    let mut b_opts = open(&b_dir, POSITIONED, network("b"));
    b_opts.identity = Some(b_identity);
    let b = reopen(b_opts)?;
    sync(&b).await;
    sync(&a).await;
    sync(&b).await;
    for device in [&a, &b] {
        assert_eq!(
            rows(device, POSITIONS).await,
            json!([
                [snap::cell(text("apples")), snap::cell(text("apples"))],
                [snap::cell(text("bread")), snap::cell(text("bread"))]
            ]),
            "the owner's close wins and the issuer's migration is never merged"
        );
        let tags = rows(device, TAGS).await;
        assert_eq!(
            tags.as_array().map(Vec::len),
            Some(2),
            "no migrated row is there twice: {tags}"
        );
        assert_eq!(generation(device).await["migration"], Value::Null);
    }
    assert_eq!(rows(&a, TAGS).await, rows(&b, TAGS).await);
    let (tip, _) = graph(&a).await;
    assert_eq!(graph(&b).await.0, tip);
    let inspected = snap::ok(b.session.inspect_tips()).await;
    let losers = inspected["bookmarks"][0]["losers"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(losers.len(), 1, "{inspected}");
    assert_eq!(losers[0]["name"], "main/1");
    assert_eq!(snap::ok(b.session.held_changes()).await["count"], 0);
    b.close().await;
    a.close().await;
    Ok(())
}

const LISTS: &str = r#"{"format":7,"application":"lists","version":1,"tables":[
 {"name":"lists","class":"replayed","columns":[{"name":"title","type":"text","not_null":true}],
  "primary_key":["id"],"partition_by":["id"],"partition_group":"list"}]}"#;
const POSITIONED_LISTS: &str = r#"{"format":7,"application":"lists","version":2,"tables":[
 {"name":"lists","class":"replayed","columns":[{"name":"title","type":"text","not_null":true},{"name":"position","type":"text","not_null":true,"default":""}],
  "primary_key":["id"],"partition_by":["id"],"partition_group":"list"}],
 "migrations":[{"version":2,"statements":["UPDATE lists SET position = title"]}]}"#;

async fn list(device: &Device, title: &str) -> Result<String, Failure> {
    snap::ok(device.session.next_partition("lists")).await;
    let mut tx = device.database.transaction().await?;
    let id = tx.next_id("lists", Vec::new()).await?;
    tx.execute(
        &format!("INSERT INTO lists(id,title) VALUES(?,'{title}')"),
        vec![id.sql()],
    )
    .await?;
    tx.commit().await?;
    Ok(id.to_string())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_issuer_without_a_partitions_key_keeps_the_households_generation() -> Result<(), Failure>
{
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let (a_dir, b_dir) = (root.path().join("a"), root.path().join("b"));
    let a = Device::new(Database::create(open(&a_dir, LISTS, network("a")), None)?)?;
    let b_opened = Database::join(
        open(&b_dir, LISTS, network("b")),
        &a.invite_as(rocky_sdk::Access::Full).await,
    )?;
    let b_identity: Arc<dyn rocky_sdk::IdentityProvider> =
        Arc::new(rocky_sdk::IdentityKey::from_phrase(
            b_opened.identity.as_deref().ok_or("linkable identity")?,
        )?);
    let b = Device::new(b_opened)?;
    a.admit(&b).await;
    introduce(&a, &b).await;
    list(&a, "From a").await?;
    let theirs = list(&b, "From b").await?;
    snap::ok(
        a.session.register_partition(
            rocky_sdk::PartitionTarget::Group {
                group: "list".to_owned(),
                values: vec![snap::row_id(&(theirs))],
            },
            rocky_sdk::PartitionSelection {
                name: None,
                omit: serde_json::from_value::<std::collections::BTreeSet<String>>(json!([]))
                    .expect("typed fixture"),
            },
        ),
    )
    .await;
    a.close().await;

    let a = reopen(open(&a_dir, POSITIONED_LISTS, network("a")))?;
    let status = generation(&a).await;
    assert_eq!(
        status["current"]["schema"], 1,
        "an issuer holding no key of a list does not sign: {status}"
    );
    assert_eq!(status["application"], "ahead");
    assert_eq!(
        status["migration"],
        json!({"state": "full_access_required"})
    );
    let query = snap::observe(snap::read(
        &a.database,
        "SELECT position FROM lists",
        Vec::new(),
    ))
    .await
    .err()
    .ok_or("a query on the new field answered")?;
    assert!(query.to_string().contains("no such column"), "{query}");

    for _ in 0..2 {
        sync(&b).await;
        sync(&a).await;
    }
    a.close().await;
    let a = reopen(open(&a_dir, POSITIONED_LISTS, network("a")))?;
    assert_eq!(
        generation(&a).await["current"]["schema"],
        2,
        "once issuers passed each other the group key, the issuer signs and migrates"
    );
    assert_eq!(
        rows(&a, "SELECT title, position FROM lists ORDER BY title").await,
        json!([
            [snap::cell(text("From a")), snap::cell(text("From a"))],
            [snap::cell(text("From b")), snap::cell(text("From b"))]
        ])
    );
    b.close().await;
    let mut b_opts = open(&b_dir, POSITIONED_LISTS, network("b"));
    b_opts.identity = Some(b_identity);
    let b = reopen(b_opts)?;
    sync(&b).await;
    sync(&b).await;
    assert_eq!(
        rows(&b, "SELECT title, position FROM lists ORDER BY title").await,
        json!([
            [snap::cell(text("From a")), snap::cell(text("From a"))],
            [snap::cell(text("From b")), snap::cell(text("From b"))]
        ])
    );
    b.close().await;
    a.close().await;
    Ok(())
}

const SEEDED: &str = r#"{"application":"shopping","version":2,"format":7,"tables":[
 {"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"note","type":"text"},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]}],
 "migrations":[{"version":2,"statements":[{"insert":"items","select":"WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 100000) SELECT printf('k%06d', i) AS key, 0 AS completed, printf('item %06d', i) AS title FROM n"}]}]}"#;
const TAGGED: &str = r#"{"application":"shopping","version":3,"format":7,"tables":[
 {"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"note","type":"text"},{"name":"tag","type":"text"},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]}],
 "migrations":[{"version":2,"statements":[{"insert":"items","select":"WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 100000) SELECT printf('k%06d', i) AS key, 0 AS completed, printf('item %06d', i) AS title FROM n"}]}]}"#;
const FILLED: &str = r#"{"application":"shopping","version":4,"format":7,"tables":[
 {"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"note","type":"text"},{"name":"position","type":"text","not_null":true,"default":""},{"name":"tag","type":"text"},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]}],
 "migrations":[{"version":2,"statements":[{"insert":"items","select":"WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 100000) SELECT printf('k%06d', i) AS key, 0 AS completed, printf('item %06d', i) AS title FROM n"}]},
  {"version":4,"statements":["UPDATE items SET position = title"]}]}"#;
const FILLED_COUNT: &str =
    "SELECT count(*), count(CASE WHEN position = title THEN 1 END) FROM items";

#[tokio::test(flavor = "multi_thread")]
async fn a_two_step_migration_converges_on_every_member() -> Result<(), Failure> {
    converges_on_every_member(8, 0).await
}

#[tokio::test(flavor = "multi_thread")]
async fn returning_member_reaches_the_newest_base_after_a_migration() -> Result<(), Failure> {
    converges_on_every_member(UNPARTITIONED_SEED_ROWS, 2).await
}

const UNPARTITIONED_SEED_ROWS: usize = 512;

struct MemberFixture {
    root: crate::support::Directory,
    large: usize,
    multi_step: bool,
    fill: String,
    hub: Arc<Hub>,
    a: Device,
}

async fn converges_on_every_member(large: usize, history: usize) -> Result<(), Failure> {
    pull_member_fixture(prepare_member_fixture(large, history).await?).await
}

async fn prepare_member_fixture(large: usize, history: usize) -> Result<MemberFixture, Failure> {
    let root = crate::support::small_directory()?;
    let multi_step = history == 0;
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
    sync(&b).await;
    let history_clock = std::time::Instant::now();
    for index in 0..history {
        snap::ok(a.session.seal()).await;
        if index == 0 {
            snap::ok(a.session.compact(0)).await;
        }
    }
    if history > 1 {
        snap::ok(a.session.compact(0)).await;
    }
    let history_time = history_clock.elapsed();
    eprintln!("STAGE rows={large} history={history_time:?}");
    a.close().await;

    let size =
        |directory: &Path| std::fs::metadata(directory.join("rocky.sqlite")).map(|m| m.len());
    b.close().await;
    let c_dir = root.path().join("publication-peer");
    let before = size(&a_dir)?;
    let clock = std::time::Instant::now();
    let seed = SEEDED.replace("100000", &large.to_string());
    let fill = if multi_step {
        FILLED.replace("100000", &large.to_string())
    } else {
        seed.clone()
    };
    let a = reopen(open(&a_dir, &seed, None))?;
    let seeded = clock.elapsed();
    eprintln!("STAGE rows={large} seed_open={seeded:?}");
    a.close().await;
    let between = size(&a_dir)?;
    if multi_step {
        let tagged_schema = TAGGED.replace("100000", &large.to_string());
        let a = reopen(open(&a_dir, &tagged_schema, None))?;
        assert_eq!(generation(&a).await["current"]["schema"], 3);
        a.close().await;
    }
    let clock = std::time::Instant::now();
    let a = reopen(open(&a_dir, &fill, network("a")))?;
    let filled = clock.elapsed();
    let open_stage = if multi_step {
        "fill_open"
    } else {
        "publisher_open"
    };
    eprintln!("STAGE rows={large} {open_stage}={filled:?}");
    let status = generation(&a).await;
    assert_eq!(
        status["current"]["schema"],
        if multi_step { 4 } else { 2 },
        "{status}"
    );
    let bundles = large.div_ceil(rocky_secure::MAX_BUNDLE_ROWS) as u64;
    let migrations = if multi_step { 2 } else { 1 };
    assert_eq!(
        status["migration"],
        json!({"state": "publishing", "bundles": migrations * bundles, "published": 0})
    );
    {
        let db = rusqlite::Connection::open_with_flags(
            a_dir.join("rocky.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let (frames, bytes, maximum): (i64, i64, i64) = db.query_row(
            "SELECT count(*),coalesce(sum(length(body)),0),coalesce(max(length(body)),0) FROM secure_operations", [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let average = bytes.checked_div(frames).unwrap_or(0);
        eprintln!(
            "FRAMES rows={large} stage={open_stage} count={frames} body_bytes={bytes} body_average={average} body_maximum={maximum}"
        );
    }
    let (ready_query, ready_rows) = if multi_step {
        (
            FILLED_COUNT,
            json!([[{"type": "integer", "value": large.to_string()}, {"type": "integer", "value": large.to_string()}]]),
        )
    } else {
        (
            "SELECT count(*) FROM items",
            json!([[{"type": "integer", "value": large.to_string()}]]),
        )
    };
    assert_eq!(rows(&a, ready_query).await, ready_rows);

    let network_bytes = hub.written();
    let publication_wire_start = *network_bytes.borrow();
    let clock = std::time::Instant::now();
    let c = Device::new(Database::join(
        open(&c_dir, &fill, network("publication-peer")),
        &a.invite().await,
    )?)?;
    sync(&c).await;
    let published = clock.elapsed();
    eprintln!("STAGE rows={large} publication={published:?}");
    let closing_wire_start = *network_bytes.borrow();
    eprintln!(
        "WIRE rows={large} publication_bytes={}",
        closing_wire_start - publication_wire_start
    );
    let clock = std::time::Instant::now();
    sync(&a).await;
    let closed = clock.elapsed();
    eprintln!("STAGE rows={large} closing_compaction={closed:?}");
    eprintln!(
        "WIRE rows={large} closing_compaction_bytes={}",
        *network_bytes.borrow() - closing_wire_start
    );
    assert_eq!(generation(&a).await["migration"], Value::Null);
    if history > 0 {
        let inspection = snap::ok(a.session.inspect_tips()).await;
        let seals = inspection["seals"].as_array().ok_or("missing seals")?;
        assert!(seals.len() >= history + 2, "only {} seals", seals.len());
        assert!(
            seals
                .iter()
                .filter(|seal| seal["checkpoints"]
                    .as_array()
                    .is_some_and(|records| !records.is_empty()))
                .count()
                >= 2,
            "the newest base must compete with an older listed base"
        );
    }
    c.close().await;
    eprintln!(
        "PREPARED rows={large} history={history_time:?} seed_open={seeded:?}  {open_stage}={filled:?} file_before={before} file_seeded={between} issuer_file={} publication={published:?} closing_compaction={closed:?}",
        size(&a_dir)?,
    );
    Ok(MemberFixture {
        root,
        large,
        multi_step,
        fill,
        hub,
        a,
    })
}

async fn pull_member_fixture(fixture: MemberFixture) -> Result<(), Failure> {
    let MemberFixture {
        root,
        large,
        multi_step,
        fill,
        hub,
        a,
    } = fixture;
    let (a_dir, b_dir) = (root.path().join("a"), root.path().join("b"));
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let (ready_query, ready_rows) = if multi_step {
        (
            FILLED_COUNT,
            json!([[{"type": "integer", "value": large.to_string()}, {"type": "integer", "value": large.to_string()}]]),
        )
    } else {
        (
            "SELECT count(*) FROM items",
            json!([[{"type": "integer", "value": large.to_string()}]]),
        )
    };
    let network_bytes = hub.written();
    let member_wire_start = *network_bytes.borrow();
    let clock = std::time::Instant::now();
    let b = reopen(open(&b_dir, &fill, network("b")))?;
    let member_open = clock.elapsed();
    eprintln!("STAGE rows={large} member_open={member_open:?}");
    let clock = std::time::Instant::now();
    let mut syncs = 0;
    while snap::observe(snap::read(&b.database, ready_query, Vec::new()))
        .await
        .ok()
        .map(|page| page["rows"].clone())
        != Some(ready_rows.clone())
    {
        syncs += 1;
        assert!(syncs <= 64, "the member did not converge");
        b.sync().await.map_err(|error| error.to_string())?;
    }
    let pulled = clock.elapsed();
    eprintln!("STAGE rows={large} member_pull={pulled:?}");

    let member_wire_bytes = *network_bytes.borrow() - member_wire_start;
    eprintln!("WIRE rows={large} member_open_and_pull_bytes={member_wire_bytes}");
    sync(&a).await;
    assert_eq!(generation(&a).await["migration"], Value::Null);
    let fingerprint = "SELECT count(*), min(title), max(title), sum(length(title)) FROM items";
    assert_eq!(rows(&a, fingerprint).await, rows(&b, fingerprint).await);
    let filled_fields = if multi_step {
        " OR position != title OR tag IS NOT NULL"
    } else {
        ""
    };
    let exact = format!(
        "SELECT count(*) FROM items WHERE key != printf('k%06d', CAST(substr(key,2) AS INTEGER)) OR CAST(substr(key,2) AS INTEGER) NOT BETWEEN 1 AND {large} OR title != printf('item %06d', CAST(substr(key,2) AS INTEGER)) OR completed != 0 OR note IS NOT NULL{filled_fields}"
    );
    for device in [&a, &b] {
        assert_eq!(
            rows(device, &exact).await,
            json!([[{"type":"integer","value":"0"}]])
        );
    }
    for directory in [&a_dir, &b_dir] {
        let db = rusqlite::Connection::open_with_flags(
            directory.join("rocky.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let (total,migration):(i64,i64)=db.query_row("SELECT bytes,(SELECT coalesce(sum(retained_bytes),0) FROM secure_migration_publications) FROM secure_retained",[],|row|Ok((row.get(0)?,row.get(1)?)))?;
        let ordinary = total
            .checked_sub(migration)
            .ok_or("invalid migration accounting")?;
        assert!((0..=67_108_864).contains(&ordinary));
        if !multi_step && directory == &a_dir {
            let mut query = db.prepare("SELECT signed FROM secure_checkpoints c WHERE sequence=(SELECT max(sequence) FROM secure_checkpoints WHERE scope=c.scope)")?;
            let mut items_base = None;
            for record in query.query_map([], |row| row.get::<_, Vec<u8>>(0))? {
                let signed = rocky_secure::SignedCheckpoint::from_bytes(&record?)?;
                if signed.statement.rows.count == large as u64 {
                    items_base = Some(signed.statement.seal);
                }
            }
            let seal = items_base.ok_or("missing closing items base")?;
            assert!(seal.is_some(), "closing items base has no seal");
        }
        eprintln!(
            "ACCOUNT rows={large} directory={} total={total} migration={migration} ordinary={ordinary}",
            directory.display()
        );
    }
    assert_eq!(syncs, 1);

    b.close().await;
    a.close().await;
    if large == 8 {
        let mut later: Value = serde_json::from_str(&fill)?;
        later["version"] = json!(5);
        later["tables"][0]["columns"]
            .as_array_mut()
            .ok_or("columns")?
            .push(json!({"name":"later", "type":"text"}));
        let later = later.to_string();
        let a = reopen(open(&a_dir, &later, None))?;
        assert_eq!(
            generation(&a).await["migration"],
            Value::Null,
            "a completed migration reopened after a later schema switch"
        );
        a.close().await;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_member_writes_its_shared_partition_after_following_a_migration() -> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let (a_dir, b_dir) = (root.path().join("a"), root.path().join("b"));
    let a = Device::new(Database::create(open(&a_dir, LISTS, network("a")), None)?)?;
    let b = Device::new(Database::join(
        open(&b_dir, LISTS, network("b")),
        &a.invite().await,
    )?)?;
    introduce(&a, &b).await;
    let id = list(&a, "Weekly").await?;
    let node = b.node().await;
    snap::ok(
        a.session.share_partition(
            rocky_sdk::PartitionTarget::Group {
                group: "list".to_owned(),
                values: vec![snap::row_id(&(id))],
            },
            serde_json::from_value::<std::collections::BTreeSet<rocky_credentials::PublicKey>>(
                json!([node]),
            )
            .expect("typed fixture"),
            None,
        ),
    )
    .await;
    sync(&b).await;
    a.close().await;
    let a = reopen(open(&a_dir, POSITIONED_LISTS, network("a")))?;
    b.close().await;
    let member_on_the_first_version = LISTS;
    let b = reopen(open(&b_dir, member_on_the_first_version, network("b")))?;
    introduce(&a, &b).await;
    sync(&b).await;
    assert_eq!(generation(&b).await["current"]["schema"], 2);
    assert_eq!(generation(&b).await["application"], "behind");
    let mut tx = b.database.transaction().await?;
    tx.execute(
        &format!("UPDATE lists SET title='Monthly' WHERE id=X'{id}'"),
        Vec::new(),
    )
    .await?;
    tx.commit().await?;
    sync(&b).await;
    assert_eq!(
        rows(&a, "SELECT title, position FROM lists").await,
        json!([[snap::cell(text("Monthly")), snap::cell(text("Weekly"))]]),
        "a member behind the migration follows main onto its line and its write arrives"
    );
    b.close().await;
    a.close().await;
    Ok(())
}

struct DescriptorIdentity {
    provider: rocky_sdk::FileIdentityProvider,
    attempts: std::sync::Mutex<Vec<Vec<u8>>>,
}
impl rocky_sdk::IdentityProvider for DescriptorIdentity {
    fn public_key(&self) -> Result<rocky_credentials::PublicKey, rocky_credentials::Error> {
        self.provider.public_key()
    }
    fn sign(
        &self,
        purpose: rocky_sdk::Purpose,
        message: &[u8],
    ) -> Result<rocky_credentials::Signature, rocky_credentials::Error> {
        let label = b"rocky.migration-publication.v1";
        if message.windows(label.len()).any(|part| part == label) {
            self.attempts.lock().unwrap().push(message.to_vec());
            return Err(rocky_credentials::Error::Provider(
                "signer unavailable".into(),
            ));
        }
        self.provider.sign(purpose, message)
    }
}
#[tokio::test(flavor = "multi_thread")]
async fn a_migration_uses_its_node_signer_and_reopening_does_not_repeat_sql() -> Result<(), Failure>
{
    let root = crate::support::small_directory()?;
    let directory = root.path().join("database");
    let identity = Arc::new(DescriptorIdentity {
        provider: rocky_sdk::FileIdentityProvider::open(root.path().join("identity"))?,
        attempts: Default::default(),
    });
    let options = |schema| Open {
        identity: Some(identity.clone() as Arc<dyn rocky_sdk::IdentityProvider>),
        ..open(&directory, schema, None)
    };
    let a = Device::new(Database::create(options(common::SCHEMA), None)?)?;
    a.add(1, "once").await;
    a.close().await;
    let a = Device::new(Database::open(options(POSITIONED))?)?;
    assert_eq!(
        rows(&a, POSITIONS).await,
        json!([[snap::cell(text("once")), snap::cell(text("once"))]])
    );
    a.close().await;
    let a = Device::new(Database::open(options(POSITIONED))?)?;
    assert_eq!(
        rows(&a, POSITIONS).await,
        json!([[snap::cell(text("once")), snap::cell(text("once"))]])
    );
    a.close().await;
    assert!(identity.attempts.lock().unwrap().is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_member_replays_a_migration_from_its_implicit_genesis_base() -> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let source = root.path().join("source");
    let a = Device::create(&source, network("a"))?;
    a.add(1, "inherited").await;
    a.close().await;
    let a = reopen(open(&source, POSITIONED, network("a")))?;
    let invitation = a.invite().await;
    let b = Device::new(Database::join(
        open(&root.path().join("member"), POSITIONED, network("b")),
        &invitation,
    )?)?;
    introduce(&a, &b).await;
    for _ in 0..4 {
        b.sync().await.map_err(|e| e.to_string())?;
        if rows(&b, POSITIONS).await
            == json!([[snap::cell(text("inherited")), snap::cell(text("inherited"))]])
        {
            break;
        }
    }
    assert_eq!(
        rows(&b, POSITIONS).await,
        json!([[snap::cell(text("inherited")), snap::cell(text("inherited"))]])
    );
    b.close().await;
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_migration_open_drives_maintenance_before_the_first_request() -> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let a = Device::create(root.path(), None)?;
    a.add(1, "apples").await;
    a.close().await;
    let finalized = || -> Result<i64, Failure> {
        let db = rusqlite::Connection::open_with_flags(
            root.path().join("rocky.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        Ok(
            db.query_row("SELECT count(*) FROM secure_finalized", [], |row| {
                row.get(0)
            })?,
        )
    };
    let before = finalized()?;
    let database = Database::open(open(root.path(), POSITIONED, None))?;
    assert!(
        finalized()? > before,
        "open left its migration seal for a later request"
    );
    let a = Device::new(database)?;
    assert_eq!(
        rows(&a, POSITIONS).await,
        json!([[snap::cell(text("apples")), snap::cell(text("apples"))]])
    );
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_member_imports_an_earlier_schemas_checkpoint_before_the_migration_tail()
-> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let source = root.path().join("source");
    let a = Device::create(&source, None)?;
    a.close().await;
    let a = reopen(open(&source, &SEEDED.replace("100000", "8"), None))?;
    a.close().await;
    let a = reopen(open(&source, &TAGGED.replace("100000", "8"), None))?;
    snap::ok(a.session.compact(0)).await;
    a.close().await;
    let hub = Arc::new(Hub::default());
    let schema = FILLED.replace("100000", "8");
    let a = reopen(open(
        &source,
        &schema,
        Some(Network::Owned(hub.connector("a"))),
    ))?;
    let c = Device::new(Database::join(
        open(
            &root.path().join("member"),
            &schema,
            Some(Network::Owned(hub.connector("c"))),
        ),
        &a.invite().await,
    )?)?;
    c.sync().await.map_err(|e| e.to_string())?;
    assert_eq!(
        rows(&c, FILLED_COUNT).await,
        json!([[{"type":"integer","value":"8"},{"type":"integer","value":"8"}]])
    );
    c.close().await;
    a.close().await;
    Ok(())
}

const LISTED: &str = r#"{"application":"shopping","version":1,"format":7,"tables":[
 {"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"list","type":"blob"},{"name":"title","type":"text","not_null":true}],"primary_key":["key"],"foreign_keys":[{"columns":["list"],"table":"lists","on_delete":"set_null"}]},
 {"name":"lists","class":"replayed","columns":[{"name":"name","type":"text","not_null":true},{"name":"first","type":"blob"}],"primary_key":["id"],"foreign_keys":[{"columns":["first"],"table":"items","on_delete":"set_null"}]}]}"#;
const UNLISTED_FAILING: &str = r#"{"application":"shopping","version":2,"format":7,"tables":[
 {"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"list","type":"blob"},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]},
 {"name":"lists","class":"replayed","columns":[{"name":"name","type":"text","not_null":true},{"name":"first","type":"blob"}],"primary_key":["id"],"foreign_keys":[{"columns":["first"],"table":"items","on_delete":"set_null"}]}],
 "migrations":[{"version":2,"statements":["UPDATE items SET title = NULL"]}]}"#;

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_migration_that_drops_a_reference_withdraws_to_the_schema_holding_it()
-> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let a = Device::new(Database::create(open(root.path(), LISTED, None), None)?)?
        .with_identity(common::external_identity(root.path()));
    a.add(1, "apples").await;
    a.close().await;

    let failed = Database::open(open(root.path(), UNLISTED_FAILING, None))
        .err()
        .ok_or("a failing migration opened")?;
    assert!(
        failed
            .to_string()
            .contains("migration to version 2 failed at statement 1"),
        "withdrawing the stage returns to the schema whose reference its rows satisfy: {failed}"
    );
    let a = reopen(open(root.path(), LISTED, None))?;
    let status = generation(&a).await;
    assert_eq!(status["current"]["schema"], 1, "{status}");
    assert_eq!(status["application"], "current");
    assert_eq!(a.titles().await, ["apples"]);
    a.close().await;
    Ok(())
}

const UNTITLED_FAILING: &str = r#"{"application":"shopping","version":2,"format":7,"tables":[
 {"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true}],"primary_key":["key"]}],
 "migrations":[{"version":2,"statements":["UPDATE items SET completed = NULL"]}]}"#;

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_migration_that_drops_a_required_field_reopens_on_the_source_schema()
-> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let a = Device::create(root.path(), None)?;
    a.add(1, "apples").await;
    a.close().await;

    let failed = common::confirmed(open(root.path(), UNTITLED_FAILING, None))
        .err()
        .ok_or("a failing migration opened")?;
    assert!(
        failed
            .to_string()
            .contains("migration to version 2 failed at statement 1"),
        "the withdrawal restores the dropped field from history instead of refusing: {failed}"
    );
    let a = reopen(open(root.path(), common::SCHEMA, None))?;
    let status = generation(&a).await;
    assert_eq!(status["current"]["schema"], 1, "{status}");
    assert_eq!(status["application"], "current");
    assert_eq!(status["migration"], Value::Null, "{status}");
    assert_eq!(a.titles().await, ["apples"]);
    a.close().await;
    Ok(())
}
