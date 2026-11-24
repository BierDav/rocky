use super::*;
use crate::support::snapshot as snap;

fn positioned_items() -> Schema {
    build(shopping(2).table(items().column(position())).migration(
        2,
        [
            "UPDATE items SET position = position || title".into(),
            MigrationStep::insert(
                "items",
                "SELECT 'copy-' || key AS key, 1 AS completed, title FROM items",
            ),
        ],
    ))
}

async fn insert(device: &Device, key: &str, title: &str) {
    let mut tx = device.database.transaction().await.expect("begin");
    let id = tx.next_id("items", Vec::new()).await.expect("id");
    tx.execute(
        "INSERT INTO items(key,completed,title,id) VALUES(?,0,?,?)",
        vec![text(key), text(title), id.sql()],
    )
    .await
    .expect("insert");
    tx.commit().await.expect("commit");
}

const STATE: &str = "SELECT id, key, completed, position, title FROM items ORDER BY id";

#[tokio::test(flavor = "multi_thread")]
async fn a_member_replays_the_issuers_program() -> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let (a_dir, b_dir) = (root.path().join("a"), root.path().join("b"));
    let a = Device::new(
        common::create_as(
            open(&a_dir, &common::schema(), network("a")),
            common::external_key(&a_dir),
        )
        .await?,
    )?;
    let b = Device::new(
        open(&b_dir, &common::schema(), network("b"))
            .join(&a.invite().await)
            .await?,
    )?;
    introduce(&a, &b).await;
    insert(&a, "k1", "apples").await;
    insert(&a, "k2", "bread").await;
    sync(&b).await;
    a.close().await;
    b.close().await;

    let a = Device::new(
        open(&a_dir, &positioned_items(), network("a"))
            .open()
            .await?,
    )?;
    assert_eq!(generation(&a).await["current"]["schema"], 2);
    let b = Device::new(
        open(&b_dir, &positioned_items(), network("b"))
            .open()
            .await?,
    )?;
    introduce(&a, &b).await;
    for device in [&b, &a, &b] {
        sync(device).await;
    }
    assert_eq!(rows(&a, STATE).await, rows(&b, STATE).await);
    assert_eq!(
        rows(&b, "SELECT key, title, position FROM items ORDER BY key").await,
        json!([
            [
                snap::cell(text("copy-k1")),
                snap::cell(text("apples")),
                snap::cell(text(""))
            ],
            [
                snap::cell(text("copy-k2")),
                snap::cell(text("bread")),
                snap::cell(text(""))
            ],
            [
                snap::cell(text("k1")),
                snap::cell(text("apples")),
                snap::cell(text("apples"))
            ],
            [
                snap::cell(text("k2")),
                snap::cell(text("bread")),
                snap::cell(text("bread"))
            ]
        ])
    );
    a.close().await;
    b.close().await;
    Ok(())
}

const LISTS: usize = 128;
const FEW_LISTS: usize = 17;
fn partitioned_items() -> Table {
    Table::replayed("items")
        .column(required("list", Affinity::Text))
        .column(required("key", Affinity::Text))
        .column(required("completed", Affinity::Integer))
        .column(Column::new("note", Affinity::Text))
        .column(required("title", Affinity::Text))
        .primary_key(["list", "key"])
        .partition_by(["list"])
}
fn partitioned(version: u32, rows: usize, lists: usize) -> Schema {
    let mut table = partitioned_items();
    if version >= 3 {
        table = table.column(Column::new("tag", Affinity::Text));
    }
    if version >= 4 {
        table = table.column(position());
    }
    let mut schema = shopping(version).table(table);
    if version >= 2 {
        schema = schema.migration(
            2,
            [MigrationStep::insert(
                "items",
                format!(
                    "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {rows}) SELECT printf('l%03d', i % {lists}) AS list, printf('k%06d', i) AS key, 0 AS completed, printf('item %06d', i) AS title FROM n"
                ),
            )],
        );
    }
    if version >= 4 {
        schema = schema.migration(4, ["UPDATE items SET position = title"]);
    }
    build(schema)
}
fn list(index: usize) -> Vec<rocky_sdk::SqlValue> {
    vec![rocky_sdk::SqlValue::Text(format!("l{index:03}"))]
}

struct Listed {
    root: crate::support::Directory,
    hub: Arc<Hub>,
    lists: usize,
}
impl Listed {
    fn path(&self, device: &str) -> std::path::PathBuf {
        self.root.path().join(device)
    }
    async fn open(&self, device: &str, schema: &Schema) -> Result<Device, Failure> {
        Device::new(
            open(
                &self.path(device),
                schema,
                Some(Network::Owned(self.hub.connector(device))),
            )
            .open()
            .await?,
        )
    }
    async fn create(lists: usize) -> Result<Self, Failure> {
        let listed = Self {
            root: crate::support::small_directory()?,
            hub: Arc::new(Hub::default()),
            lists,
        };
        let network = |name: &str| Some(Network::Owned(listed.hub.connector(name)));
        let a = Device::new(
            common::create_as(
                open(&listed.path("a"), &partitioned(1, 0, 0), network("a")),
                common::external_key(&listed.path("a")),
            )
            .await?,
        )?;
        let b = Device::new(
            open(&listed.path("b"), &partitioned(1, 0, 0), network("b"))
                .join(&a.invite().await)
                .await?,
        )?;
        introduce(&a, &b).await;
        for index in 0..lists {
            snap::ok(
                a.database
                    .create_partition(&rocky_sdk::Partition::new("items", (list(index)).clone())),
            )
            .await;
        }
        sync(&b).await;
        a.close().await;
        b.close().await;
        Ok(listed)
    }
    async fn seed(&self, rows: usize) -> Result<(), Failure> {
        for version in [2, 3] {
            let a = Device::new(
                open(
                    &self.path("a"),
                    &partitioned(version, rows, self.lists),
                    None,
                )
                .open()
                .await?,
            )?;
            a.close().await;
        }
        Ok(())
    }
    async fn migrate(&self, rows: usize) -> Result<std::time::Duration, Failure> {
        self.seed(rows).await?;
        self.fill(rows).await
    }
    async fn fill(&self, rows: usize) -> Result<std::time::Duration, Failure> {
        let clock = std::time::Instant::now();
        let a = self.open("a", &partitioned(4, rows, self.lists)).await?;
        let filled = clock.elapsed();
        assert_eq!(generation(&a).await["current"]["schema"], 4);
        a.close().await;
        Ok(filled)
    }
    async fn pull(&self, rows: usize) -> Result<std::time::Duration, Failure> {
        let schema = partitioned(4, rows, self.lists);
        let a = self.open("a", &schema).await?;
        let clock = std::time::Instant::now();
        let b = self.open("b", &schema).await?;
        let expected = json!([[{"type": "integer", "value": rows.to_string()}, {"type": "integer", "value": rows.to_string()}]]);
        let mut syncs = 0;
        while snap::observe(snap::read(&b.database, FILLED_COUNT, Vec::new()))
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

#[tokio::test(flavor = "multi_thread")]
#[ignore = "release-only measurement, ROCKY_PROFILE_FILL=<file> profiling the first fill on macOS: cargo test --release -p rocky-sdk --test integration fill_of_100000 -- --ignored --nocapture"]
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

const CRASH_ROWS: usize = 256;

#[tokio::test(flavor = "multi_thread")]
#[ignore = "invoked by an_interrupted_fill_resumes_and_converges"]
async fn crash_fill_child() -> Result<(), Failure> {
    let directory = std::env::var("ROCKY_CRASH_DIRECTORY")?;
    let a = Device::new(
        open(
            Path::new(&directory),
            &partitioned(4, CRASH_ROWS, FEW_LISTS),
            None,
        )
        .open()
        .await?,
    )?;
    a.close().await;
    Ok(())
}

#[derive(Debug, PartialEq)]
struct PublicationState {
    stage: String,
    cursor: Option<(i64, i64)>,
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

#[tokio::test(flavor = "multi_thread")]
async fn an_interrupted_fill_resumes_and_converges() -> Result<(), Failure> {
    let chunk = rocky_secure::PUBLICATION_CHUNK_BUNDLES as i64;
    let lists = FEW_LISTS as i64;
    for (crash_point, cursor_left, authored_frames_left) in [
        ("after", (0, lists), 0),
        ("publication-staged", (0, lists), 0),
        ("publication-chunk", (chunk, lists), chunk),
        ("publication-between", (chunk, lists), chunk),
        ("publication-final", (lists, lists), lists),
    ] {
        let listed = Listed::create(FEW_LISTS).await?;
        listed.seed(CRASH_ROWS).await?;
        let status = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "migrations::replayed::crash_fill_child",
                "--ignored",
            ])
            .env("ROCKY_SQL_KILL_AT", crash_point)
            .env("ROCKY_CRASH_DIRECTORY", listed.path("a"))
            .status()?;
        assert_eq!(status.code(), Some(87), "{crash_point}");
        assert_eq!(
            publication_state(&listed.path("a"))?,
            PublicationState {
                stage: "staged".into(),
                cursor: Some(cursor_left),
                unsent: (authored_frames_left, authored_frames_left),
            },
            "{crash_point}"
        );
        listed.pull(CRASH_ROWS).await?;
        let [issuer, member] = listed.digests().await?;
        assert_eq!(issuer.count, CRASH_ROWS as u64, "{crash_point}");
        assert_eq!(issuer, member, "{crash_point}");
        let db = rusqlite::Connection::open_with_flags(
            listed.path("a").join("rocky.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let (publications, bundles, frames): (i64, i64, i64) = db.query_row(
            "SELECT count(*),sum(bundles),(SELECT count(*) FROM secure_publication_frames) FROM secure_publications",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(
            (publications, bundles, frames),
            (2, 2 * lists, 2 * lists),
            "seed and fill publish one bundle of one frame per partition each after {crash_point}"
        );
    }
    Ok(())
}
