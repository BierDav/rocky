//! Migrations of replayed tables publish their program: each partition's
//! copy replays on every member as its own transaction.
use super::*;

/// Replayed items, unpartitioned.
const ITEMS: &str = r#"{"application":"shopping","version":1,"format":6,"tables":[
 {"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]}]}"#;
/// Requires `position`, filled from the title, and copies each item.
const POSITIONED_ITEMS: &str = r#"{"application":"shopping","version":2,"format":6,"tables":[
 {"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"position","type":"text","not_null":true,"default":""},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]}],
 "migrations":[{"version":2,"statements":["UPDATE items SET position = position || title",
  {"insert":"items","select":"SELECT 'copy-' || key AS key, 1 AS completed, title FROM items"}]}]}"#;

async fn insert(device: &Device, key: &str, title: &str) {
    device.ok(json!({"type": "sql", "sql": "BEGIN"})).await;
    let id = device
        .ok(json!({"type": "next_id", "table": "items"}))
        .await["id"]
        .clone();
    device
        .ok(json!({"type": "sql", "sql": "INSERT INTO items(key,completed,title,id) VALUES(?,0,?,?)", "values": [text(key), text(title), {"type": "integer", "value": id}]}))
        .await;
    device.ok(json!({"type": "sql", "sql": "COMMIT"})).await;
}

const STATE: &str = "SELECT id, key, completed, position, title FROM items ORDER BY id";

#[tokio::test(flavor = "multi_thread")]
async fn a_member_replays_the_issuers_program() -> Result<(), Failure> {
    let root = common::identity::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let (a_dir, b_dir) = (root.path().join("a"), root.path().join("b"));
    let a = Device::new(Database::create(open(&a_dir, ITEMS, network("a")), None)?)?;
    a.follow_main_migrating().await;
    let b = Device::new(Database::join(
        open(&b_dir, ITEMS, network("b")),
        &a.invite().await,
    )?)?;
    introduce(&a, &b).await;
    insert(&a, "k1", "apples").await;
    insert(&a, "k2", "bread").await;
    sync(&b).await;
    a.close().await;
    b.close().await;

    let a = Device::new(Database::open(open(
        &a_dir,
        POSITIONED_ITEMS,
        network("a"),
    ))?)?;
    assert_eq!(generation(&a).await["current"]["schema"], 2);
    let b = Device::new(Database::open(open(
        &b_dir,
        POSITIONED_ITEMS,
        network("b"),
    ))?)?;
    introduce(&a, &b).await;
    for device in [&b, &a, &b] {
        sync(device).await;
    }
    assert_eq!(rows(&a, STATE).await, rows(&b, STATE).await);
    assert_eq!(
        rows(&b, "SELECT key, title, position FROM items ORDER BY key").await,
        json!([
            [text("copy-k1"), text("apples"), text("")],
            [text("copy-k2"), text("bread"), text("")],
            [text("k1"), text("apples"), text("apples")],
            [text("k2"), text("bread"), text("bread")]
        ])
    );
    a.close().await;
    b.close().await;
    Ok(())
}

/// Partitions of the replayed 100k fixture: each partition's seed intent
/// fits one pulled frame.
const LISTS: usize = 128;
/// Partitions of the correctness fixtures: three authoring chunks.
const FEW_LISTS: usize = 17;
/// The replayed fixture: items in `LISTS` partitions, seeded by an
/// `Insert` step, then filled by an `UPDATE` step two versions later.
const LISTED: &str = r#"{"application":"shopping","version":1,"format":6,"tables":[
 {"name":"items","class":"replayed","columns":[{"name":"list","type":"text","not_null":true},{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"note","type":"text"},{"name":"title","type":"text","not_null":true}],"primary_key":["list","key"],"partition_by":["list"]}]}"#;
const LISTED_SEED: &str = r#"{"version":2,"statements":[{"insert":"items","select":"WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 100000) SELECT printf('l%03d', i % LISTS) AS list, printf('k%06d', i) AS key, 0 AS completed, printf('item %06d', i) AS title FROM n"}]}"#;
const LISTED_SEEDED: &str = r#"{"application":"shopping","version":2,"format":6,"tables":[
 {"name":"items","class":"replayed","columns":[{"name":"list","type":"text","not_null":true},{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"note","type":"text"},{"name":"title","type":"text","not_null":true}],"primary_key":["list","key"],"partition_by":["list"]}],
 "migrations":[SEED]}"#;
/// `LISTED_SEEDED` with a nullable tag: an additive switch in place.
const LISTED_TAGGED: &str = r#"{"application":"shopping","version":3,"format":6,"tables":[
 {"name":"items","class":"replayed","columns":[{"name":"list","type":"text","not_null":true},{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"note","type":"text"},{"name":"tag","type":"text"},{"name":"title","type":"text","not_null":true}],"primary_key":["list","key"],"partition_by":["list"]}],
 "migrations":[SEED]}"#;
/// Requires `position` of every item, filled from its title.
const LISTED_FILLED: &str = r#"{"application":"shopping","version":4,"format":6,"tables":[
 {"name":"items","class":"replayed","columns":[{"name":"list","type":"text","not_null":true},{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"note","type":"text"},{"name":"position","type":"text","not_null":true,"default":""},{"name":"tag","type":"text"},{"name":"title","type":"text","not_null":true}],"primary_key":["list","key"],"partition_by":["list"]}],
 "migrations":[SEED,{"version":4,"statements":["UPDATE items SET position = title"]}]}"#;

fn listed(schema: &str, rows: usize, lists: usize) -> String {
    schema
        .replace("SEED", LISTED_SEED)
        .replace("100000", &rows.to_string())
        .replace("LISTS", &lists.to_string())
}
fn list(index: usize) -> Value {
    json!([{"type": "text", "value": format!("l{index:03}")}])
}

/// An issuer and a member holding every partition, closed after the
/// member's first sync.
struct Listed {
    root: common::identity::Directory,
    hub: Arc<Hub>,
    lists: usize,
}
impl Listed {
    fn path(&self, device: &str) -> std::path::PathBuf {
        self.root.path().join(device)
    }
    fn open(&self, device: &str, schema: &str) -> Result<Device, Failure> {
        Device::new(Database::open(open(
            &self.path(device),
            schema,
            Some(Network::Owned(self.hub.connector(device))),
        ))?)
    }
    async fn create(lists: usize) -> Result<Self, Failure> {
        let listed = Self {
            root: common::identity::small_directory()?,
            hub: Arc::new(Hub::default()),
            lists,
        };
        let network = |name: &str| Some(Network::Owned(listed.hub.connector(name)));
        let a = Device::new(Database::create(
            open(&listed.path("a"), LISTED, network("a")),
            None,
        )?)?;
        a.follow_main_migrating().await;
        let b = Device::new(Database::join(
            open(&listed.path("b"), LISTED, network("b")),
            &a.invite().await,
        )?)?;
        introduce(&a, &b).await;
        let node = b.node().await;
        for index in 0..lists {
            a.ok(json!({"type": "partition_create", "table": "items", "values": list(index)}))
                .await;
            a.ok(json!({"type": "partition_share", "table": "items", "values": list(index), "recipients": [node]}))
                .await;
        }
        sync(&b).await;
        a.close().await;
        b.close().await;
        Ok(listed)
    }
    /// Runs the seed and the additive switch on the issuer.
    async fn seed(&self, rows: usize) -> Result<(), Failure> {
        for schema in [LISTED_SEEDED, LISTED_TAGGED] {
            let a = Device::new(Database::open(open(
                &self.path("a"),
                &listed(schema, rows, self.lists),
                None,
            ))?)?;
            a.close().await;
        }
        Ok(())
    }
    /// Runs the seed, the additive switch and the fill on the issuer and
    /// returns how long the fill's open took.
    async fn migrate(&self, rows: usize) -> Result<std::time::Duration, Failure> {
        self.seed(rows).await?;
        self.fill(rows).await
    }
    /// Opens the fill's schema on the issuer; returns how long that took.
    async fn fill(&self, rows: usize) -> Result<std::time::Duration, Failure> {
        let clock = std::time::Instant::now();
        let a = self.open("a", &listed(LISTED_FILLED, rows, self.lists))?;
        let filled = clock.elapsed();
        assert_eq!(generation(&a).await["current"]["schema"], 4);
        a.close().await;
        Ok(filled)
    }
    /// The member opens the fill's schema and syncs until it holds every
    /// filled row; returns how long that took.
    async fn pull(&self, rows: usize) -> Result<std::time::Duration, Failure> {
        let schema = listed(LISTED_FILLED, rows, self.lists);
        let a = self.open("a", &schema)?;
        let clock = std::time::Instant::now();
        let b = self.open("b", &schema)?;
        let expected = json!([[{"type": "integer", "value": rows.to_string()}, {"type": "integer", "value": rows.to_string()}]]);
        let mut syncs = 0;
        while b
            .call(json!({"type": "read", "query": {"sql": FILLED_COUNT, "values": [], "max_rows": 1}}))
            .await
            .ok()
            .map(|page| page["rows"].clone())
            != Some(expected.clone())
        {
            syncs += 1;
            assert!(syncs <= 64, "the member did not converge");
            sync(&b).await;
        }
        let pulled = clock.elapsed();
        sync(&a).await;
        assert_eq!(generation(&a).await["migration"], Value::Null);
        a.close().await;
        b.close().await;
        Ok(pulled)
    }
    /// The projection of `items` on the issuer and the member.
    async fn digests(&self) -> Result<[rocky_secure::SetSummary; 2], Failure> {
        let mut digests = Vec::new();
        for device in ["a", "b"] {
            let mut tables = rocky_secure::local_table_digests(
                &self.path(device).join("rocky.sqlite"),
                &["items"],
            )
            .await?;
            digests.push(tables.remove("items").ok_or("items digest")?);
        }
        Ok([digests[0], digests[1]])
    }
}

/// Each partition's member replay of the seed and the fill reproduces the
/// issuer's rows, identities included.
#[tokio::test(flavor = "multi_thread")]
async fn a_member_replays_each_partition_of_the_issuers_rows() -> Result<(), Failure> {
    let rows = 64;
    let listed = Listed::create(FEW_LISTS).await?;
    listed.migrate(rows).await?;
    listed.pull(rows).await?;
    let [issuer, member] = listed.digests().await?;
    assert_eq!(issuer.count, rows as u64);
    assert_eq!(issuer, member);
    Ok(())
}

/// The largest `-wal` size of `files` seen until the returned flag is set.
fn sample_wal(
    files: Vec<std::path::PathBuf>,
) -> (
    Arc<std::sync::atomic::AtomicBool>,
    std::thread::JoinHandle<u64>,
) {
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop = done.clone();
    let sampler = std::thread::spawn(move || {
        let mut peak = 0;
        while !stop.load(std::sync::atomic::Ordering::SeqCst) {
            for file in &files {
                peak = peak.max(std::fs::metadata(file).map_or(0, |meta| meta.len()));
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        peak
    });
    (done, sampler)
}

/// The replayed 100k fixture, three times: issuer fill, member pull, file
/// sizes and peak WAL; the member's digest equals the issuer's. With
/// `ROCKY_PROFILE_FILL=<file>`, macOS `sample` profiles the first fill.
/// Diagnostic; `cargo test --release -p rocky-sdk --test migrations
/// replayed::fill_of_100000 -- --ignored --nocapture`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "measurement"]
async fn fill_of_100000_replayed_rows() -> Result<(), Failure> {
    if cfg!(debug_assertions) {
        return Err("run measurements with --release".into());
    }
    let rows = 100_000;
    for trial in 0..3 {
        let listed = Listed::create(LISTS).await?;
        let wal = || {
            ["a", "b"]
                .map(|device| listed.path(device).join("rocky.sqlite-wal"))
                .to_vec()
        };
        let (done, sampler) = sample_wal(wal());
        listed.seed(rows).await?;
        done.store(true, std::sync::atomic::Ordering::SeqCst);
        let seed_wal = sampler.join().map_err(|_| "WAL sampler")?;
        let (done, sampler) = sample_wal(wal());
        let profile = std::env::var_os("ROCKY_PROFILE_FILL")
            .filter(|_| trial == 0)
            .map(|file| {
                std::process::Command::new("sample")
                    .args([std::process::id().to_string(), "8".into(), "-file".into()])
                    .arg(file)
                    .spawn()
            })
            .transpose()?;
        let filled = listed.fill(rows).await?;
        done.store(true, std::sync::atomic::Ordering::SeqCst);
        let fill_wal = sampler.join().map_err(|_| "WAL sampler")?;
        if let Some(mut profile) = profile {
            profile.wait()?;
        }
        // With ROCKY_MEASURE_PUBLICATION_PAGES set: each authoring
        // transaction's bundles and WAL pages, seed's then fill's.
        for (bundles, pages) in rocky_secure::CHUNK_PAGES
            .lock()
            .map_err(|_| "chunk pages")?
            .drain(..)
        {
            eprintln!("CHUNK bundles={bundles} pages={pages}");
        }
        let (done, sampler) = sample_wal(wal());
        let pulled = listed.pull(rows).await?;
        done.store(true, std::sync::atomic::Ordering::SeqCst);
        let pull_wal = sampler.join().map_err(|_| "WAL sampler")?;
        let [issuer, member] = listed.digests().await?;
        assert_eq!(issuer.count, rows as u64);
        assert_eq!(issuer, member);
        let size = |device: &str| {
            std::fs::metadata(listed.path(device).join("rocky.sqlite")).map(|meta| meta.len())
        };
        eprintln!(
            "REPLAYED trial={trial} rows={rows} fill_open={filled:?} member_pull={pulled:?} issuer_file={} member_file={} peak_wal_seed={seed_wal} peak_wal_fill={fill_wal} peak_wal_pull={pull_wal}",
            size("a")?,
            size("b")?
        );
    }
    Ok(())
}

/// Rows of the crash fixture: one chunk-sized run of partitions after another.
const CRASH_ROWS: usize = 256;

/// Opens the fill's schema on the issuer in `ROCKY_CRASH_DIRECTORY`; the
/// armed `ROCKY_SQL_KILL_AT` point ends the process.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "invoked by an_interrupted_fill_resumes_and_converges"]
async fn crash_fill_child() -> Result<(), Failure> {
    let directory = std::env::var("ROCKY_CRASH_DIRECTORY")?;
    let a = Device::new(Database::open(open(
        Path::new(&directory),
        &listed(LISTED_FILLED, CRASH_ROWS, FEW_LISTS),
        None,
    ))?)?;
    a.close().await;
    Ok(())
}

/// The issuer's publication state.
#[derive(Debug, PartialEq)]
struct PublicationState {
    stage: String,
    /// The next position and the total.
    cursor: Option<(i64, i64)>,
    /// Frames still unsent, and how many of them some bundle authored.
    unsent: (i64, i64),
}

fn publication_state(path: &Path) -> Result<PublicationState, Failure> {
    let db = rusqlite::Connection::open_with_flags(
        path.join("rocky.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let stage = db
        .query_row(
            "SELECT state FROM secure_staged_schema WHERE state!='released'",
            [],
            |row| row.get(0),
        )
        .unwrap_or_else(|_| "released".into());
    let cursor = db
        .query_row(
            "SELECT next,total FROM secure_publication_cursors",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok();
    let unsent = db.query_row(
        "SELECT count(*) FROM secure_publication_unsent",
        [],
        |row| row.get(0),
    )?;
    let unsent_authored = db.query_row(
        "SELECT count(*) FROM secure_publication_unsent u JOIN secure_operations o ON o.hash=u.hash",
        [],
        |row| row.get(0),
    )?;
    Ok(PublicationState {
        stage,
        cursor,
        unsent: (unsent, unsent_authored),
    })
}

/// A fill killed at each point of its publication reopens, resumes from its
/// cursor without authoring a bundle twice, and every member converges.
/// Nothing authored left the device before the crash.
#[tokio::test(flavor = "multi_thread")]
async fn an_interrupted_fill_resumes_and_converges() -> Result<(), Failure> {
    let chunk = rocky_secure::PUBLICATION_CHUNK_BUNDLES as i64;
    let lists = FEW_LISTS as i64;
    // Each point, with the cursor and authored frames the crash leaves.
    for (point, cursor, authored) in [
        ("after", (0, lists), 0),
        ("publication-staged", (0, lists), 0),
        ("publication-chunk", (chunk, lists), chunk),
        ("publication-between", (chunk, lists), chunk),
        ("publication-final", (lists, lists), lists),
    ] {
        let listed = Listed::create(FEW_LISTS).await?;
        listed.seed(CRASH_ROWS).await?;
        let status = std::process::Command::new(std::env::current_exe()?)
            .args(["--exact", "replayed::crash_fill_child", "--ignored"])
            .env("ROCKY_SQL_KILL_AT", point)
            .env("ROCKY_CRASH_DIRECTORY", listed.path("a"))
            .status()?;
        assert_eq!(status.code(), Some(87), "{point}");
        assert_eq!(
            publication_state(&listed.path("a"))?,
            PublicationState {
                stage: "staged".into(),
                cursor: Some(cursor),
                unsent: (authored, authored),
            },
            "{point}"
        );
        listed.pull(CRASH_ROWS).await?;
        let [issuer, member] = listed.digests().await?;
        assert_eq!(issuer.count, CRASH_ROWS as u64, "{point}");
        assert_eq!(issuer, member, "{point}");
        let db = rusqlite::Connection::open_with_flags(
            listed.path("a").join("rocky.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        // Seed and fill: one bundle of one frame per partition each.
        let (publications, bundles, frames): (i64, i64, i64) = db.query_row(
            "SELECT count(*),sum(bundles),(SELECT count(*) FROM secure_publication_frames) FROM secure_publications",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(
            (publications, bundles, frames),
            (2, 2 * lists, 2 * lists),
            "{point}"
        );
    }
    Ok(())
}
