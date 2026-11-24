use crate::common;
mod prepared_pull;
mod replayed;
use crate::support::snapshot as snap;
use common::{Device, Failure, build, introduce, items, required, shopping};
use rocky_sdk::{
    Affinity, Column, DeleteAction, ForeignKeyDefinition, MigrationStep, Network, Schema,
    SchemaBuilder, Table, transport::memory::Hub,
};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

fn position() -> Column {
    Column::new("position", Affinity::Text)
        .required()
        .default("")
}
fn tags() -> Table {
    Table::replayed("tags")
        .column(required("key", Affinity::Text))
        .column(required("label", Affinity::Text))
        .primary_key(["key"])
}
fn tagged_items(version: u32) -> SchemaBuilder {
    shopping(version)
        .table(items().column(position()))
        .table(tags())
}
fn positioning(tag_key: &str) -> [MigrationStep; 2] {
    [
        "UPDATE items SET position = position || title".into(),
        MigrationStep::insert(
            "tags",
            format!("SELECT {tag_key} AS key, title AS label FROM items"),
        ),
    ]
}
fn positioned() -> Schema {
    build(tagged_items(2).migration(2, positioning("'tag-' || key")))
}
fn asks_for_ids() -> Schema {
    build(tagged_items(2).migration(2, ["UPDATE items SET position = rocky_next_id('items')"]))
}
fn fixed() -> Schema {
    build(tagged_items(3).migration(2, positioning("'tag-' || key")))
}

async fn reopen(
    directory: &Path,
    schema: &Schema,
    network: Option<Network>,
) -> Result<Device, Failure> {
    reopen_as(
        directory,
        schema,
        network,
        common::external_identity(directory),
    )
    .await
}

async fn reopen_as(
    directory: &Path,
    schema: &Schema,
    network: Option<Network>,
    signer: Arc<dyn rocky_sdk::IdentityProvider>,
) -> Result<Device, Failure> {
    let configuration = open(directory, schema, network).identity(signer.clone());
    Device::new(configuration.open().await?)
}

fn open(directory: &Path, schema: &Schema, network: Option<Network>) -> rocky_sdk::Configuration {
    crate::common::configuration(
        directory,
        schema,
        None,
        Some(common::external_identity(directory)),
        network,
    )
}
async fn sync(device: &Device) {
    let Some(report) = crate::support::peerless_ok(device.sync_now().await).expect("sync") else {
        return;
    };
    if std::env::var_os("ROCKY_TRACE_MIGRATION").is_some() {
        eprintln!("migration report: {report}");
    }
    for failure in report
        .peers
        .iter()
        .flat_map(|peer| &peer.attempts)
        .filter_map(|attempt| attempt.failure.as_ref())
    {
        eprintln!("migration synchronization: {failure:?}");
    }
}
async fn generation(device: &Device) -> Value {
    snap::ok(async {
        device
            .database
            .status()
            .await
            .map(|status| status.data.generation)
    })
    .await
}
async fn rows(device: &Device, sql: &str) -> Value {
    snap::ok(snap::read(&device.database, sql, Vec::new())).await["rows"].clone()
}
fn text(value: &str) -> rocky_sdk::SqlValue {
    rocky_sdk::SqlValue::Text(snap::text(&(value)))
}
const POSITIONS: &str = "SELECT title, position FROM items ORDER BY title";
const TAGS: &str = "SELECT key, label FROM tags ORDER BY key";

fn duplicates() -> Schema {
    build(tagged_items(2).migration(2, positioning("'same'")))
}

async fn graph(device: &Device) -> (Value, usize) {
    let tip = snap::ok(device.database.tip_status()).await["tip"].clone();
    let seals = snap::ok(device.database.inspect_tips()).await["seals"]
        .as_array()
        .map_or(0, Vec::len);
    (tip, seals)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_migration_runs_again_and_a_committed_one_never() -> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let a = Device::create(root.path(), None).await?;
    a.add(1, "apples").await;
    a.add(2, "bread").await;
    let before = graph(&a).await;
    a.close().await;

    let refused = open(root.path(), &asks_for_ids(), None)
        .open()
        .await
        .err()
        .ok_or("a migration asking for ids opened")?;
    assert!(
        refused.to_string().contains("rocky_next_id"),
        "a migration asking for an id is refused before anything is signed: {refused}"
    );
    let failed = open(root.path(), &duplicates(), None)
        .open()
        .await
        .err()
        .ok_or("a failing migration opened")?;
    assert!(failed_at(&failed, 2, 2), "{failed}");
    assert!(
        !failed.retryable(),
        "rerunning the same migration fails alike"
    );
    let a = reopen(root.path(), &common::schema(), None).await?;
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

    let a = reopen(root.path(), &fixed(), None).await?;
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

    let a = reopen(root.path(), &fixed(), None).await?;
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
    let a = Device::create(&a_dir, network("a")).await?;
    let b_opened = open(&b_dir, &common::schema(), network("b"))
        .join(&a.invite_as(rocky_sdk::Access::Full).await)
        .await?;
    let b_identity = common::external_identity(&b_dir);
    let b = Device::new(b_opened.database)?;
    a.admit(&b).await;
    introduce(&a, &b).await;
    a.add(1, "apples").await;
    b.add(2, "bread").await;
    sync(&b).await;
    a.close().await;
    b.close().await;

    for (directory, id) in [(&a_dir, None), (&b_dir, Some(b_identity.clone()))] {
        let device = match id {
            Some(id) => reopen_as(directory, &positioned(), None, id).await?,
            None => reopen(directory, &positioned(), None).await?,
        };
        assert_eq!(
            generation(&device).await["current"]["schema"],
            2,
            "apart, each issuer signs the generation and migrates what it holds"
        );
        device.close().await;
    }
    let a = reopen(&a_dir, &positioned(), network("a")).await?;
    let b = reopen_as(&b_dir, &positioned(), network("b"), b_identity).await?;
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
    let inspected = snap::ok(b.database.inspect_tips()).await;
    let losers = inspected["bookmarks"][0]["losers"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(losers.len(), 1, "{inspected}");
    assert_eq!(losers[0]["name"], "main/1");
    assert_eq!(snap::ok(b.database.held_changes()).await["count"], 0);
    b.close().await;
    a.close().await;
    Ok(())
}

fn lists_table() -> Table {
    Table::replayed("lists")
        .column(required("title", Affinity::Text))
        .partition_by(["id"])
        .partition_group("list")
}
fn lists() -> Schema {
    build(Schema::builder("lists", 1).table(lists_table()))
}
fn positioned_lists() -> Schema {
    build(
        Schema::builder("lists", 2)
            .table(lists_table().column(position()))
            .migration(2, ["UPDATE lists SET position = title"]),
    )
}

async fn list(device: &Device, title: &str) -> Result<String, Failure> {
    snap::ok(device.database.next_partition("lists")).await;
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
async fn an_issuer_migrates_the_household_once_a_members_list_key_reaches_it() -> Result<(), Failure>
{
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let (a_dir, b_dir) = (root.path().join("a"), root.path().join("b"));
    let a = Device::new(
        common::create_as(
            open(&a_dir, &lists(), network("a")),
            common::external_key(&a_dir),
        )
        .await?,
    )?;
    let b_opened = open(&b_dir, &lists(), network("b"))
        .join(&a.invite_as(rocky_sdk::Access::Full).await)
        .await?;
    let b_identity = common::external_identity(&b_dir);
    let b = Device::new(b_opened.database)?;
    a.admit(&b).await;
    introduce(&a, &b).await;
    list(&a, "From a").await?;
    list(&b, "From b").await?;
    for _ in 0..2 {
        sync(&b).await;
        sync(&a).await;
    }
    a.close().await;
    let a = reopen(&a_dir, &positioned_lists(), network("a")).await?;
    for _ in 0..2 {
        sync(&b).await;
        sync(&a).await;
    }
    a.close().await;
    let a = reopen(&a_dir, &positioned_lists(), network("a")).await?;
    assert_eq!(
        generation(&a).await["current"]["schema"],
        2,
        "the member's creation hands its key to the issuer, which migrates every list"
    );
    assert_eq!(
        rows(&a, "SELECT title, position FROM lists ORDER BY title").await,
        json!([
            [snap::cell(text("From a")), snap::cell(text("From a"))],
            [snap::cell(text("From b")), snap::cell(text("From b"))]
        ])
    );
    b.close().await;
    let b = reopen_as(&b_dir, &positioned_lists(), network("b"), b_identity).await?;
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

fn noted_items() -> Table {
    items().column(Column::new("note", Affinity::Text))
}
fn tagged_noted_items() -> Table {
    noted_items().column(Column::new("tag", Affinity::Text))
}
fn seed(rows: usize) -> [MigrationStep; 1] {
    [MigrationStep::insert(
        "items",
        format!(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {rows}) SELECT printf('k%06d', i) AS key, 0 AS completed, printf('item %06d', i) AS title FROM n"
        ),
    )]
}
fn seeded(rows: usize) -> Schema {
    build(shopping(2).table(noted_items()).migration(2, seed(rows)))
}
fn tagged(rows: usize) -> Schema {
    build(
        shopping(3)
            .table(tagged_noted_items())
            .migration(2, seed(rows)),
    )
}
fn filling(version: u32, rows: usize, table: Table) -> Schema {
    build(
        shopping(version)
            .table(table)
            .migration(2, seed(rows))
            .migration(4, ["UPDATE items SET position = title"]),
    )
}
fn filled(rows: usize) -> Schema {
    filling(4, rows, tagged_noted_items().column(position()))
}
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
    fill: Schema,
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
    let a = Device::create(&a_dir, network("a")).await?;
    let b = Device::join(
        &b_dir,
        &a.invite().await,
        Network::Owned(hub.connector("b")),
    )
    .await?;
    introduce(&a, &b).await;
    sync(&b).await;
    let history_clock = std::time::Instant::now();
    for index in 0..history {
        snap::ok(a.database.seal()).await;
        if index == 0 {
            snap::ok(a.database.compact(0)).await;
        }
    }
    if history > 1 {
        snap::ok(a.database.compact(0)).await;
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
    let seed = seeded(large);
    let fill = if multi_step {
        filled(large)
    } else {
        seed.clone()
    };
    let a = reopen(&a_dir, &seed, None).await?;
    let seeded = clock.elapsed();
    eprintln!("STAGE rows={large} seed_open={seeded:?}");
    a.close().await;
    let between = size(&a_dir)?;
    if multi_step {
        let a = reopen(&a_dir, &tagged(large), None).await?;
        assert_eq!(generation(&a).await["current"]["schema"], 3);
        a.close().await;
    }
    let clock = std::time::Instant::now();
    let a = reopen(&a_dir, &fill, network("a")).await?;
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
    let c = Device::new(
        open(&c_dir, &fill, network("publication-peer"))
            .join(&a.invite().await)
            .await?,
    )?;
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
        let inspection = snap::ok(a.database.inspect_tips()).await;
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
    let b = reopen(&b_dir, &fill, network("b")).await?;
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
        let later = filling(
            5,
            large,
            tagged_noted_items()
                .column(position())
                .column(Column::new("later", Affinity::Text)),
        );
        let a = reopen(&a_dir, &later, None).await?;
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
    let a = Device::new(
        common::create_as(
            open(&a_dir, &lists(), network("a")),
            common::external_key(&a_dir),
        )
        .await?,
    )?;
    let b = Device::new(
        open(&b_dir, &lists(), network("b"))
            .join(&a.invite().await)
            .await?,
    )?;
    introduce(&a, &b).await;
    let id = list(&a, "Weekly").await?;
    sync(&b).await;
    a.close().await;
    let a = reopen(&a_dir, &positioned_lists(), network("a")).await?;
    b.close().await;
    let member_on_the_first_version = lists();
    let b = reopen(&b_dir, &member_on_the_first_version, network("b")).await?;
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
    let options = |schema: &Schema| {
        crate::common::configuration(
            &directory,
            schema,
            None,
            Some(identity.clone() as Arc<dyn rocky_sdk::IdentityProvider>),
            None,
        )
    };
    let a = Device::new(
        options(&common::schema())
            .create_with_root(
                Arc::new(rocky_sdk::RootKey::generate()),
                identity.clone() as Arc<dyn rocky_sdk::IdentityProvider>,
            )
            .await?,
    )?;
    a.add(1, "once").await;
    a.close().await;
    let a = Device::new(options(&positioned()).open().await?)?;
    assert_eq!(
        rows(&a, POSITIONS).await,
        json!([[snap::cell(text("once")), snap::cell(text("once"))]])
    );
    a.close().await;
    let a = Device::new(options(&positioned()).open().await?)?;
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
    let a = Device::create(&source, network("a")).await?;
    a.add(1, "inherited").await;
    a.close().await;
    let a = reopen(&source, &positioned(), network("a")).await?;
    let invitation = a.invite().await;
    let b = Device::new(
        open(&root.path().join("member"), &positioned(), network("b"))
            .join(&invitation)
            .await?,
    )?;
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
    let a = Device::create(root.path(), None).await?;
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
    let database = open(root.path(), &positioned(), None).open().await?;
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
    let a = Device::create(&source, None).await?;
    a.close().await;
    let a = reopen(&source, &seeded(8), None).await?;
    a.close().await;
    let a = reopen(&source, &tagged(8), None).await?;
    snap::ok(a.database.compact(0)).await;
    a.close().await;
    let hub = Arc::new(Hub::default());
    let schema = filled(8);
    let a = reopen(&source, &schema, Some(Network::Owned(hub.connector("a")))).await?;
    let c = Device::new(
        open(
            &root.path().join("member"),
            &schema,
            Some(Network::Owned(hub.connector("c"))),
        )
        .join(&a.invite().await)
        .await?,
    )?;
    c.sync().await.map_err(|e| e.to_string())?;
    assert_eq!(
        rows(&c, FILLED_COUNT).await,
        json!([[{"type":"integer","value":"8"},{"type":"integer","value":"8"}]])
    );
    c.close().await;
    a.close().await;
    Ok(())
}

fn set_null(column: &str, table: &str) -> ForeignKeyDefinition {
    ForeignKeyDefinition {
        columns: vec![column.into()],
        table: table.into(),
        on_delete: DeleteAction::SetNull,
        deferred: false,
    }
}
fn listed_items() -> Table {
    items().column(Column::new("list", Affinity::Blob))
}
fn first_items() -> Table {
    Table::replayed("lists")
        .column(required("name", Affinity::Text))
        .column(Column::new("first", Affinity::Blob))
        .foreign_key(set_null("first", "items"))
}
fn listed() -> Schema {
    build(
        shopping(1)
            .table(listed_items().foreign_key(set_null("list", "lists")))
            .table(first_items()),
    )
}
fn unlisted_failing() -> Schema {
    build(
        shopping(2)
            .table(listed_items())
            .table(first_items())
            .migration(2, ["UPDATE items SET title = NULL"]),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_migration_that_drops_a_reference_withdraws_to_the_schema_holding_it()
-> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let a = Device::new(
        common::create_as(
            open(root.path(), &listed(), None),
            common::external_key(root.path()),
        )
        .await?,
    )?;
    a.add(1, "apples").await;
    a.close().await;

    let failed = open(root.path(), &unlisted_failing(), None)
        .open()
        .await
        .err()
        .ok_or("a failing migration opened")?;
    assert!(
        failed_at(&failed, 2, 1),
        "withdrawing the stage returns to the schema whose reference its rows satisfy: {failed}"
    );
    let a = reopen(root.path(), &listed(), None).await?;
    let status = generation(&a).await;
    assert_eq!(status["current"]["schema"], 1, "{status}");
    assert_eq!(status["application"], "current");
    assert_eq!(a.titles().await, ["apples"]);
    a.close().await;
    Ok(())
}

fn untitled_items() -> Table {
    Table::replayed("items")
        .column(required("key", Affinity::Text))
        .column(required("completed", Affinity::Integer))
        .primary_key(["key"])
}
fn untitled_failing() -> Schema {
    build(
        shopping(2)
            .table(untitled_items())
            .migration(2, ["UPDATE items SET completed = NULL"]),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_migration_that_drops_a_required_field_reopens_on_the_source_schema()
-> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let a = Device::create(root.path(), None).await?;
    a.add(1, "apples").await;
    a.close().await;

    let failed = common::confirmed(open(root.path(), &untitled_failing(), None))
        .await
        .err()
        .ok_or("a failing migration opened")?;
    assert!(
        failed_at(&*failed, 2, 1),
        "the withdrawal restores the dropped field from history instead of refusing: {failed}"
    );
    let a = reopen(root.path(), &common::schema(), None).await?;
    let status = generation(&a).await;
    assert_eq!(status["current"]["schema"], 1, "{status}");
    assert_eq!(status["application"], "current");
    assert_eq!(status["migration"], Value::Null, "{status}");
    assert_eq!(a.titles().await, ["apples"]);
    a.close().await;
    Ok(())
}

fn memo() -> Column {
    Column::new("memo", Affinity::Text).required().default("")
}
const MEMO_FROM_TITLE: [&str; 1] = ["UPDATE items SET memo = title"];
fn memoed() -> Schema {
    build(
        shopping(2)
            .table(items().column(memo()))
            .migration(2, MEMO_FROM_TITLE),
    )
}
fn untitled() -> Schema {
    build(
        shopping(3)
            .table(untitled_items().column(memo()))
            .migration(2, MEMO_FROM_TITLE),
    )
}
const MEMOS: &str = "SELECT key, memo FROM items ORDER BY key";

async fn skipping_versions_converges(late_write: bool) -> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let dirs = |name: &str| root.path().join(name);
    let owner = Device::create(&dirs("owner"), network("owner")).await?;
    let bob = Device::join(
        &dirs("bob"),
        &owner.invite().await,
        Network::Owned(hub.connector("bob")),
    )
    .await?;
    let carol = Device::join(
        &dirs("carol"),
        &owner.invite().await,
        Network::Owned(hub.connector("carol")),
    )
    .await?;
    introduce(&owner, &bob).await;
    introduce(&owner, &carol).await;
    let coffee = owner.add(1, "Coffee").await;
    sync(&bob).await;
    sync(&carol).await;
    assert_eq!(bob.titles().await, ["Coffee"]);
    bob.close().await;
    owner.close().await;

    let owner = reopen(&dirs("owner"), &memoed(), network("owner")).await?;
    sync(&carol).await;
    assert_eq!(generation(&carol).await["current"]["schema"], 2);
    owner.close().await;
    let owner =
        Device::new(common::confirmed(open(&dirs("owner"), &untitled(), network("owner"))).await?)?;
    assert_eq!(generation(&owner).await["current"]["schema"], 3);
    sync(&carol).await;
    let migrated = json!([[snap::cell(text(&coffee)), snap::cell(text("Coffee"))]]);
    assert_eq!(rows(&owner, MEMOS).await, migrated);

    let bob = reopen(&dirs("bob"), &common::schema(), network("bob")).await?;
    if late_write {
        bob.add(2, "Tea").await;
    }
    introduce(&owner, &bob).await;
    for _ in 0..3 {
        sync(&bob).await;
        sync(&carol).await;
        sync(&owner).await;
    }
    assert_eq!(generation(&bob).await["current"]["schema"], 3);
    let expected = rows(&owner, MEMOS).await;
    if !late_write {
        assert_eq!(expected, migrated);
    }
    assert_eq!(
        rows(&bob, MEMOS).await,
        expected,
        "a follower skipping a version replays the skipped migration's writes"
    );
    assert_eq!(rows(&carol, MEMOS).await, expected);
    bob.close().await;

    let bob = reopen(&dirs("bob"), &untitled(), network("bob")).await?;
    sync(&bob).await;
    assert_eq!(generation(&bob).await["application"], "current");
    assert_eq!(rows(&bob, MEMOS).await, expected);
    bob.close().await;
    carol.close().await;
    owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_follower_skipping_versions_replays_every_intermediate_migration() -> Result<(), Failure>
{
    skipping_versions_converges(false).await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_late_write_from_a_follower_skipping_versions_lands_alike_on_every_device()
-> Result<(), Failure> {
    skipping_versions_converges(true).await
}

fn unmemoed() -> Schema {
    build(shopping(2).table(untitled_items()))
}
const KEPT: &str = "SELECT key, completed FROM items ORDER BY key";

async fn a_member_joining_after(rollouts: &[Schema], read: &str) -> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let dirs = |name: &str| root.path().join(name);
    let owner = Device::create(&dirs("owner"), network("owner")).await?;
    owner.add(1, "Coffee").await;
    owner.add(2, "Tea").await;
    owner.add(3, "Water").await;
    owner.close().await;
    for (index, schema) in rollouts.iter().enumerate() {
        let configuration = open(&dirs("owner"), schema, None);
        let opened = match index + 1 == rollouts.len() {
            true => common::confirmed(configuration).await?,
            false => configuration.open().await?,
        };
        Device::new(opened)?.close().await;
    }
    let last = rollouts.last().cloned().unwrap_or_else(common::schema);
    let owner = reopen(&dirs("owner"), &last, network("owner")).await?;
    let held = rows(&owner, read).await;
    assert_eq!(held.as_array().map(Vec::len), Some(3), "{held}");
    let member = Device::new(
        open(&dirs("member"), &last, network("member"))
            .join(&owner.invite().await)
            .await?,
    )?;
    introduce(&owner, &member).await;
    for _ in 0..3 {
        sync(&member).await;
    }
    assert_eq!(
        rows(&member, read).await,
        held,
        "a member joining after a removal starts from the removing seal's checkpoint"
    );
    member.close().await;
    owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_member_joining_after_a_removal_holds_every_row_the_owner_holds() -> Result<(), Failure> {
    a_member_joining_after(&[unmemoed()], KEPT).await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_member_joining_after_a_migration_and_a_removal_holds_every_migrated_row()
-> Result<(), Failure> {
    a_member_joining_after(&[memoed(), untitled()], MEMOS).await
}

fn failed_at(error: &(dyn std::error::Error + 'static), version: u32, step: usize) -> bool {
    matches!(
        error.downcast_ref::<rocky_sdk::Error>().map(rocky_sdk::Error::kind),
        Some(rocky_sdk::ErrorKind::MigrationFailed { version: failed, step: at })
            if *failed == version && *at == step
    )
}

async fn dropped_alerts(device: &Device) -> Result<Vec<rocky_sdk::TransactionToken>, Failure> {
    Ok(device
        .database
        .status()
        .await?
        .alerts
        .into_iter()
        .filter_map(|alert| match alert.kind {
            rocky_sdk::status::AlertKind::WriteDropped {
                token,
                reason: rocky_sdk::DropReason::Schema,
                ..
            } => Some(token),
            _ => None,
        })
        .collect())
}

struct LateWriter {
    root: crate::support::Directory,
    hub: Arc<Hub>,
    owner: Device,
    bob: Device,
    carol: Device,
    events: rocky_sdk::TableEvents,
    tea: rocky_sdk::TransactionToken,
}
impl LateWriter {
    fn path(&self, name: &str) -> std::path::PathBuf {
        self.root.path().join(name)
    }
    fn network(&self, name: &str) -> Option<Network> {
        Some(Network::Owned(self.hub.connector(name)))
    }
    async fn create() -> Result<Self, Failure> {
        let root = crate::support::small_directory()?;
        let hub = Arc::new(Hub::default());
        let network = |name: &str| Some(Network::Owned(hub.connector(name)));
        let dirs = |name: &str| root.path().join(name);
        let owner = Device::create(&dirs("owner"), network("owner")).await?;
        let bob = Device::join(
            &dirs("bob"),
            &owner.invite().await,
            Network::Owned(hub.connector("bob")),
        )
        .await?;
        let carol = Device::join(
            &dirs("carol"),
            &owner.invite().await,
            Network::Owned(hub.connector("carol")),
        )
        .await?;
        introduce(&owner, &bob).await;
        introduce(&owner, &carol).await;
        owner.add(1, "Coffee").await;
        sync(&bob).await;
        sync(&carol).await;
        bob.close().await;
        owner.close().await;

        let owner = reopen(&dirs("owner"), &memoed(), network("owner")).await?;
        sync(&carol).await;
        owner.close().await;
        let owner = Device::new(
            common::confirmed(open(&dirs("owner"), &untitled(), network("owner"))).await?,
        )?;
        sync(&carol).await;

        let bob = reopen(&dirs("bob"), &common::schema(), network("bob")).await?;
        let events = bob.database.observe(&[]).await?;
        let mut tx = bob.database.transaction().await?;
        let id = tx.next_id("items", Vec::new()).await?;
        tx.execute(
            "INSERT INTO items(id,key,completed,title) VALUES(?,'tea',0,'Tea')",
            vec![id.sql()],
        )
        .await?;
        let rocky_sdk::Commit::Recorded(tea) = tx.commit().await? else {
            return Err("a write has a token".into());
        };
        introduce(&owner, &bob).await;
        for _ in 0..3 {
            sync(&bob).await;
            sync(&carol).await;
            sync(&owner).await;
        }
        assert_eq!(generation(&bob).await["current"]["schema"], 3);
        Ok(Self {
            root,
            hub,
            owner,
            bob,
            carol,
            events,
            tea,
        })
    }
    async fn close(self) {
        self.bob.close().await;
        self.carol.close().await;
        self.owner.close().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_late_write_a_removal_no_longer_applies_is_dropped_loudly() -> Result<(), Failure> {
    let mut late = LateWriter::create().await?;
    let tea = late.tea;
    let dropped = rocky_sdk::DroppedTransaction {
        token: tea,
        reason: rocky_sdk::DropReason::Schema,
    };
    while !late.events.next().await?.dropped.contains(&dropped) {}
    assert_eq!(
        late.bob.database.transaction_outcome(tea).await?,
        Some(rocky_sdk::TransactionOutcome::Dropped {
            reason: rocky_sdk::DropReason::Schema,
            sealed: false,
        })
    );
    assert_eq!(
        dropped_alerts(&late.bob).await?,
        [tea],
        "the writer is told its write was dropped"
    );
    assert_eq!(
        dropped_alerts(&late.owner).await?,
        [tea],
        "an admin is told a member's write was dropped"
    );
    assert_eq!(
        dropped_alerts(&late.carol).await?,
        [],
        "a member that neither wrote it nor administers stays quiet"
    );

    late.bob.close().await;
    late.bob = reopen(&late.path("bob"), &untitled(), late.network("bob")).await?;
    assert_eq!(
        dropped_alerts(&late.bob).await?,
        [tea],
        "the alert survives a restart until it is acknowledged"
    );
    late.bob.database.acknowledge_drop(tea).await?;
    assert_eq!(dropped_alerts(&late.bob).await?, []);
    late.bob.close().await;
    late.bob = reopen(&late.path("bob"), &untitled(), late.network("bob")).await?;
    assert_eq!(
        dropped_alerts(&late.bob).await?,
        [],
        "an acknowledged drop stays acknowledged"
    );
    assert_eq!(
        dropped_alerts(&late.owner).await?,
        [tea],
        "each device acknowledges for itself"
    );
    late.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_application_behind_a_removal_it_uses_must_upgrade() -> Result<(), Failure> {
    let mut late = LateWriter::create().await?;
    let upgrade = rocky_sdk::ErrorKind::UpgradeRequired {
        current: 3,
        declared: 1,
    };
    let mut tx = late.bob.database.transaction().await?;
    let id = tx.next_id("items", Vec::new()).await?;
    let refused = tx
        .execute(
            "INSERT INTO items(id,key,completed,title) VALUES(?,'milk',0,'Milk')",
            vec![id.sql()],
        )
        .await
        .err()
        .ok_or("a write into the removed field succeeded")?;
    assert_eq!(
        *refused.kind(),
        upgrade,
        "SQL naming the removed field says the application must upgrade: {refused}"
    );
    assert!(!refused.retryable());
    drop(tx);
    late.bob.close().await;

    let refused = open(&late.path("bob"), &common::schema(), late.network("bob"))
        .open()
        .await
        .err()
        .ok_or("an application behind a removal it uses opened")?;
    assert_eq!(*refused.kind(), upgrade, "{refused}");
    assert!(!refused.retryable());
    late.bob = reopen(&late.path("bob"), &unmemoed(), late.network("bob")).await?;
    assert_eq!(
        generation(&late.bob).await["application"],
        "behind",
        "an application behind that uses only what is kept opens"
    );
    late.close().await;
    Ok(())
}

async fn reported_schemas(admin: &Device) -> Result<Vec<(String, Option<u32>)>, Failure> {
    let mut reported: Vec<(String, Option<u32>)> = admin
        .database
        .people()
        .await?
        .into_iter()
        .flat_map(|person| person.devices)
        .map(|device| {
            (
                device.id.to_string(),
                device.schema.map(|schema| schema.version),
            )
        })
        .collect();
    reported.sort();
    Ok(reported)
}

async fn until_reported(
    admin: &Device,
    device: &Device,
    version: u32,
) -> Result<Vec<(String, Option<u32>)>, Failure> {
    let node = device.database.me().await?.device.to_string();
    for _ in 0..8 {
        sync(device).await;
        let reported = reported_schemas(admin).await?;
        if reported.contains(&(node.clone(), Some(version))) {
            return Ok(reported);
        }
    }
    Err(format!("{node} never reported schema version {version}").into())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_admin_sees_the_schema_version_each_device_last_reported() -> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let dirs = |name: &str| root.path().join(name);
    let owner = Device::create(&dirs("owner"), network("owner")).await?;
    let bob = Device::join(
        &dirs("bob"),
        &owner.invite().await,
        Network::Owned(hub.connector("bob")),
    )
    .await?;
    introduce(&owner, &bob).await;
    let own = owner.database.me().await?.device.to_string();
    let bobs = bob.database.me().await?.device.to_string();
    assert_eq!(until_reported(&owner, &bob, 1).await?, {
        let mut both = vec![(own.clone(), Some(1)), (bobs.clone(), Some(1))];
        both.sort();
        both
    });
    owner.close().await;

    let owner = reopen(&dirs("owner"), &memoed(), network("owner")).await?;
    let reported = reported_schemas(&owner).await?;
    assert!(
        reported.contains(&(own.clone(), Some(2))) && reported.contains(&(bobs.clone(), None)),
        "a device that has not reported since this admin opened is unknown: {reported:?}"
    );
    assert!(
        until_reported(&owner, &bob, 1)
            .await?
            .contains(&(bobs.clone(), Some(1)))
    );
    bob.close().await;
    let bob = reopen(&dirs("bob"), &memoed(), network("bob")).await?;
    introduce(&owner, &bob).await;
    until_reported(&owner, &bob, 2).await?;
    bob.close().await;
    owner.close().await;
    Ok(())
}

fn waits_for_removal_checkpoint(blocker: &rocky_sdk::status::Blocker) -> bool {
    matches!(
        blocker.kind,
        rocky_sdk::status::BlockerKind::SealRebootstrap {
            replay: rocky_sdk::status::FrameReplay::Never,
            ..
        }
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_past_a_removal_waits_for_an_admin_and_its_reads_say_why() -> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let dirs = |name: &str| root.path().join(name);
    let owner = Device::create(&dirs("owner"), network("owner")).await?;
    let bob = Device::join(
        &dirs("bob"),
        &owner.invite().await,
        Network::Owned(hub.connector("bob")),
    )
    .await?;
    let person: Arc<dyn rocky_sdk::IdentityProvider> = Arc::new(rocky_sdk::IdentityKey::generate());
    let invitation = owner
        .database
        .invite(
            rocky_sdk::Invite::new(rocky_sdk::Access::Store)
                .for_identity(person.public_key()?.into()),
        )
        .await?;
    let relay = Device::new(
        rocky_sdk::Configuration::store_only(dirs("relay"))
            .sync(rocky_sdk::SyncMode::Manual)
            .network(Network::Owned(hub.connector("relay")))
            .identity(person)
            .join(&invitation.code)
            .await?,
    )?;
    introduce(&owner, &bob).await;
    introduce(&owner, &relay).await;
    owner.add(1, "Coffee").await;
    sync(&bob).await;
    bob.close().await;
    owner.close().await;
    let owner =
        Device::new(common::confirmed(open(&dirs("owner"), &unmemoed(), network("owner"))).await?)?;
    sync(&relay).await;
    owner.close().await;

    let bob = reopen(&dirs("bob"), &unmemoed(), network("bob")).await?;
    introduce(&relay, &bob).await;
    sync(&bob).await;
    let blockers = bob.database.status().await?.data.blockers;
    let waiting: Vec<_> = blockers
        .iter()
        .filter(|blocker| waits_for_removal_checkpoint(blocker))
        .cloned()
        .collect();
    assert_eq!(
        waiting.len(),
        1,
        "a relay cannot serve the checkpoint: {blockers:?}"
    );
    assert_eq!(waiting[0].clears_by, rocky_sdk::status::ClearsBy::Admin);
    let explained = waiting[0].to_string();
    assert!(
        explained.contains("removed a table or field")
            && explained.contains("the next sync with an admin device"),
        "{explained}"
    );
    let refused = bob
        .database
        .query("SELECT key FROM items", Vec::new())
        .await
        .err()
        .ok_or("a scope waiting for its checkpoint was read")?;
    let rocky_sdk::ErrorKind::Blocked {
        table: Some(table),
        blockers,
    } = refused.kind()
    else {
        return Err(format!("not blocked on a table: {refused:?}").into());
    };
    assert_eq!(table, "items");
    assert!(
        blockers.iter().any(waits_for_removal_checkpoint),
        "{blockers:?}"
    );
    assert!(refused.retryable());

    let owner = reopen(&dirs("owner"), &unmemoed(), network("owner")).await?;
    introduce(&owner, &bob).await;
    sync(&bob).await;
    assert!(
        !bob.database
            .status()
            .await?
            .data
            .blockers
            .iter()
            .any(waits_for_removal_checkpoint),
        "the next sync with an admin clears the wait"
    );
    assert_eq!(rows(&bob, KEPT).await.as_array().map(Vec::len), Some(1));
    bob.close().await;
    relay.close().await;
    owner.close().await;
    Ok(())
}
