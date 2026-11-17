use crate::common;
use crate::support::snapshot as snap;
use common::{Device, Failure, introduce};
use rocky_sdk::{Database, Network, Open, transport::memory::Hub};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

const SHELVED: &str = r#"{"application":"shopping","version":1,"format":6,"tables":[{"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"title","type":"text","not_null":true},{"name":"category","type":"text"},{"name":"shelf","type":"blob"}],"primary_key":["key"],"indexes":[{"name":"items_category","unique":true,"columns":[{"name":"category","descending":false}]}],"foreign_keys":[{"columns":["shelf"],"table":"shelves","on_delete":"set_null"}]},{"name":"shelves","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"label","type":"text","not_null":true}],"primary_key":["key"]}]}"#;
const UNSHELVED: &str = r#"{"application":"shopping","version":2,"format":6,"tables":[{"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]}]}"#;
const NOTED: &str = r#"{"application":"shopping","version":1,"format":6,"tables":[{"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"note","type":"text"},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]}]}"#;

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
fn version(schema: &str, version: u32) -> String {
    schema.replace(r#""version":1"#, &format!(r#""version":{version}"#))
}
async fn rows(device: &Device, sql: &str) -> Value {
    snap::ok(device.session.read(rocky_sdk::ReadQuery {
        sql: snap::text(&(sql)),
        values: vec![],
        max_rows: 1024,
        max_bytes: None,
        cursor: None,
    }))
    .await["rows"]
        .clone()
}
fn text(value: &str) -> rocky_sdk::SqlValue {
    rocky_sdk::SqlValue::Text(snap::text(&(value)))
}
async fn generation(device: &Device) -> Value {
    snap::ok(device.session.generation()).await
}
async fn create(device: &Device, handle: u8, collection: &str, values: Value) -> String {
    let reply = snap::ok(snap::mutate(
        &device.session,
        vec![handle; 32],
        snap::Action::Create {
            table: snap::text(&(collection)),
            values: snap::fields(values),
        },
        None,
        None,
    ))
    .await;
    reply["id"].as_str().unwrap_or_default().to_owned()
}
async fn write(device: &Device, _collection: &str, handle: u8, sql: &str) {
    snap::ok(device.session.execute("BEGIN", Vec::new())).await;
    snap::ok(device.session.execute(&(snap::text(&(sql))), Vec::new())).await;
    snap::ok(device.session.commit([handle; 32])).await;
}
fn schema_of(directory: &Path) -> Result<(Vec<String>, Vec<String>), Failure> {
    let file = rusqlite::Connection::open_with_flags(
        directory.join("rocky.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let mut statement = file.prepare(
        "SELECT name FROM sqlite_schema WHERE type IN ('table','index') AND name NOT LIKE 'secure_%' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE 'issuer_registry%' ORDER BY name",
    )?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;
    let mut statement =
        file.prepare("SELECT name FROM pragma_table_info('items') ORDER BY name")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;
    Ok((names, columns))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_generation_removes_a_field_its_unique_index_and_a_referenced_table()
-> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let (a_dir, b_dir) = (root.path().join("a"), root.path().join("b"));
    let a = Device::new(Database::create(open(&a_dir, SHELVED, network("a")), None)?)?;
    a.follow_main_migrating().await;
    let b = Device::new(Database::join(
        open(&b_dir, SHELVED, network("b")),
        &a.invite_as(json!({"kind": "writer", "scopes": ["items", "shelves"]}))
            .await,
    )?)?;
    introduce(&a, &b).await;
    create(&a, 1, "shelves", json!({"label": "pantry"})).await;
    let shelf: Vec<u8> =
        serde_json::from_value(rows(&a, "SELECT id FROM shelves").await[0][0]["value"].clone())?;
    let shelf = rocky_sdk::RowIdentity::from_bytes(&shelf)?.to_string();
    create(
        &a,
        2,
        "items",
        json!({"title": "apples", "completed": false, "category": "fruit", "shelf": shelf}),
    )
    .await;
    create(
        &a,
        3,
        "items",
        json!({"title": "bread", "completed": false, "category": "bakery"}),
    )
    .await;
    b.sync().await.expect("sync");
    assert_eq!(b.titles().await, ["apples", "bread"]);
    write(
        &b,
        "items",
        4,
        "UPDATE items SET title='green apples', category='produce' WHERE title='apples'",
    )
    .await;
    b.sync().await.expect("sync");

    a.close().await;
    let a = Device::new(common::confirmed(open(&a_dir, UNSHELVED, network("a")))?)?;
    assert_eq!(generation(&a).await["current"]["schema"], 2);
    assert_eq!(a.titles().await, ["bread", "green apples"]);
    b.close().await;
    let b = Device::new(Database::open(open(&b_dir, UNSHELVED, network("b")))?)?;
    b.sync().await.expect("sync");
    b.sync().await.expect("sync");
    for device in [&a, &b] {
        assert_eq!(generation(device).await["current"]["schema"], 2);
        assert_eq!(
            device.titles().await,
            ["bread", "green apples"],
            "a frame sent before the removal keeps its authority after the signed schema opens"
        );
    }
    let failed = snap::observe(b.session.read(rocky_sdk::ReadQuery {
        sql: "SELECT category FROM items".to_owned(),
        values: vec![],
        max_rows: 8,
        max_bytes: None,
        cursor: None,
    }))
    .await
    .expect_err("removed column");
    assert!(
        failed["message"]
            .as_str()
            .is_some_and(|message| message.contains("no such column")),
        "reading a removed column fails with an ordinary SQLite error: {failed}"
    );
    a.close().await;
    b.close().await;
    for directory in [&a_dir, &b_dir] {
        let (names, columns) = schema_of(directory)?;
        assert!(
            !names.iter().any(|name| name.contains("shelves")),
            "{names:?}"
        );
        assert!(!names.contains(&"items_category".to_string()), "{names:?}");
        assert!(names.contains(&"items".to_string()), "{names:?}");
        assert!(
            !columns
                .iter()
                .any(|name| name == "category" || name == "shelf"),
            "{columns:?}"
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_readded_field_never_receives_a_value_written_before_its_removal() -> Result<(), Failure>
{
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let directory = |name: &str| root.path().join(name);
    let a = Device::new(Database::create(
        open(&directory("a"), NOTED, network("a")),
        None,
    )?)?;
    a.follow_main_migrating().await;
    let join = |name: &str, invitation: &str| -> Result<Device, Failure> {
        Device::new(Database::join(
            open(&directory(name), NOTED, network(name)),
            invitation,
        )?)
    };
    let c = join("c", &a.invite_as(json!({"kind": "issuer"})).await)?;
    let d = join("d", &a.invite().await)?;
    let e = join("e", &a.invite().await)?;
    for member in [&c, &d, &e] {
        introduce(&a, member).await;
    }
    introduce(&c, &d).await;
    let apples = create(
        &a,
        1,
        "items",
        json!({"title": "apples", "completed": false, "note": "old"}),
    )
    .await;
    create(
        &a,
        2,
        "items",
        json!({"title": "bread", "completed": false, "note": "stale"}),
    )
    .await;
    for member in [&c, &d, &e] {
        member.sync().await.expect("sync");
    }
    for member in [&c, &d, &e] {
        snap::ok(member.session.set_networking_enabled(false)).await;
    }
    a.close().await;
    let removed = Device::new(common::confirmed(open(
        &directory("a"),
        &version(common::SCHEMA, 2),
        None,
    ))?)?;
    removed.close().await;
    let a = Device::new(Database::open(open(
        &directory("a"),
        &version(NOTED, 3),
        network("a"),
    ))?)?;
    assert_eq!(generation(&a).await["current"]["schema"], 3);
    let notes = "SELECT title, note FROM items ORDER BY title";
    let empty = json!([[snap::cell(text("apples")), {"type": "null"}], [snap::cell(text("bread")), {"type": "null"}]]);
    assert_eq!(rows(&a, notes).await, empty);
    d.set(3, &apples, "note", json!("late")).await;

    snap::ok(c.session.set_networking_enabled(true)).await;
    c.sync().await.expect("sync");
    assert_eq!(
        generation(&c).await["current"]["schema"],
        3,
        "following main from A to C at once passes B's schema on the path"
    );

    assert_eq!(rows(&c, notes).await, empty);
    snap::ok(d.session.set_networking_enabled(true)).await;
    d.sync().await.expect("sync");
    assert_eq!(generation(&d).await["current"]["schema"], 3);
    assert_eq!(
        generation(&d).await["held"],
        0,
        "a late write replays in C, where its field is off its path, and nothing is held"
    );
    for device in [&a, &c, &d] {
        device.sync().await.expect("sync");
        assert_eq!(
            rows(device, notes).await,
            empty,
            "a late write crossing B's removal reaches no device, its writer's included"
        );
    }
    a.set(4, &apples, "note", json!("current")).await;
    let expected = json!([[snap::cell(text("apples")), snap::cell(text("current"))], [snap::cell(text("bread")), {"type": "null"}]]);
    snap::ok(e.session.set_networking_enabled(true)).await;
    for device in [&c, &d, &e] {
        device.sync().await.expect("sync");
    }
    for device in [&a, &c, &d, &e] {
        assert_eq!(generation(device).await["current"]["schema"], 3);
        assert_eq!(
            rows(device, notes).await,
            expected,
            "a write at C carries the field again, and following strips older notes"
        );
    }
    for device in [&c, &d, &e, &a] {
        device.close().await;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unsent_change_naming_a_removed_table_drops_whole_in_the_successor()
-> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let (a_dir, b_dir) = (root.path().join("a"), root.path().join("b"));
    let a = Device::new(Database::create(open(&a_dir, SHELVED, network("a")), None)?)?;
    a.follow_main_migrating().await;
    let b = Device::new(Database::join(
        open(&b_dir, SHELVED, network("b")),
        &a.invite_as(json!({"kind": "writer", "scopes": ["items", "shelves"]}))
            .await,
    )?)?;
    introduce(&a, &b).await;
    create(
        &a,
        1,
        "items",
        json!({"title": "apples", "completed": false}),
    )
    .await;
    create(
        &a,
        2,
        "items",
        json!({"title": "bread", "completed": false}),
    )
    .await;
    b.sync().await.expect("sync");
    snap::ok(b.session.execute("BEGIN", Vec::new())).await;
    snap::ok(b.session.execute(
        "UPDATE items SET title='green apples' WHERE title='apples'",
        Vec::new(),
    ))
    .await;
    let id = snap::ok(b.session.next_id("shelves")).await["id"].clone();
    snap::ok(b.session.execute(
        "INSERT INTO shelves(key,label,id) VALUES('cellar','cellar',?)",
        vec![snap::row_id(&(id.to_string().trim_matches('"')))],
    ))
    .await;
    snap::ok(b.session.commit([3; 32])).await;
    assert_eq!(b.titles().await, ["bread", "green apples"]);

    a.close().await;
    let a = Device::new(common::confirmed(open(&a_dir, UNSHELVED, network("a")))?)?;
    b.close().await;
    let b = Device::new(Database::open(open(&b_dir, UNSHELVED, network("b")))?)?;
    b.sync().await.expect("sync");
    b.sync().await.expect("sync");
    assert_eq!(generation(&b).await["current"]["schema"], 2);
    assert_eq!(generation(&b).await["held"], 0);
    a.sync().await.expect("sync");
    for device in [&a, &b] {
        assert_eq!(
            device.titles().await,
            ["apples", "bread"],
            "a late transaction naming a removed table drops whole in the successor"
        );
    }

    write(
        &b,
        "items",
        4,
        "UPDATE items SET completed=1 WHERE title='bread'",
    )
    .await;
    b.sync().await.expect("sync");
    let completed = "SELECT title, completed FROM items WHERE completed=1";
    assert_eq!(
        rows(&a, completed).await,
        json!([[snap::cell(text("bread")), {"type": "integer", "value": "1"}]]),
        "the member's later edits reach the issuer"
    );
    a.close().await;
    b.close().await;
    Ok(())
}

fn confirmation(
    result: Result<rocky_sdk::Opened, Failure>,
) -> rocky_sdk::RolloutConfirmationRequired {
    *result
        .err()
        .expect("confirmation required")
        .downcast::<rocky_sdk::RolloutConfirmationRequired>()
        .expect("structured report")
}

struct NeverStarts(std::sync::atomic::AtomicBool);
impl rocky_sdk::transport::Connector for NeverStarts {
    fn start(
        &self,
        _: bool,
        _: [u8; 32],
    ) -> rocky_sdk::transport::BoxFuture<'_, Result<rocky_sdk::transport::Connected, Failure>> {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async { Err("network must not start before confirmation".into()) })
    }
}
fn files(path: &Path) -> Result<Vec<(std::path::PathBuf, String)>, Failure> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(path)? {
        let path = entry?.path();
        if path.is_dir() {
            found.extend(files(&path)?);
        } else if !path.file_name().is_some_and(|name| {
            name == "rocky.sqlite-shm"
                || (name == "rocky.sqlite-wal" && path.metadata().is_ok_and(|file| file.len() == 0))
        }) {
            found.push((
                path.clone(),
                data_encoding::HEXLOWER
                    .encode(&rocky_core::commitment("test.file", &std::fs::read(path)?)),
            ));
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(found)
}

#[tokio::test(flavor = "multi_thread")]
async fn removing_open_changes_nothing_and_only_the_current_report_can_sign() -> Result<(), Failure>
{
    let root = crate::support::small_directory()?;
    let directory = root.path();
    let a = Device::new(Database::create(open(directory, SHELVED, None), None)?)?;
    a.follow_main_migrating().await;
    create(&a, 1, "shelves", json!({"label": "pantry"})).await;
    create(
        &a,
        2,
        "items",
        json!({"title": "apples", "completed": false, "category": "fruit"}),
    )
    .await;
    a.close().await;
    let identity = std::fs::read(directory.join("identity.json"))?;
    let database = std::fs::read(directory.join("rocky.sqlite"))?;
    let before = files(directory)?;
    let network = Arc::new(NeverStarts(std::sync::atomic::AtomicBool::new(false)));
    let required = confirmation(Database::open(open(
        directory,
        UNSHELVED,
        Some(Network::Owned(network.clone())),
    )));
    assert!(!network.0.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(files(directory)?, before);
    assert!(!required.mismatch);
    assert_eq!(
        required.report.tables,
        [rocky_sdk::RemovalImpact {
            name: "shelves".into(),
            rows: 1
        }]
    );
    assert_eq!(
        required.report.fields,
        [
            rocky_sdk::RemovalImpact {
                name: "items.category".into(),
                rows: 1
            },
            rocky_sdk::RemovalImpact {
                name: "items.shelf".into(),
                rows: 1
            },
        ]
    );
    assert_eq!(std::fs::read(directory.join("identity.json"))?, identity);
    assert_eq!(std::fs::read(directory.join("rocky.sqlite"))?, database);
    let changed = UNSHELVED.replace(r#""version":2"#, r#""version":3"#);
    let stale = confirmation(Database::open_confirmed(
        open(directory, &changed, None),
        &required.report.digest,
    ));
    assert!(stale.mismatch);
    assert_ne!(stale.report.digest, required.report.digest);
    assert_eq!(std::fs::read(directory.join("identity.json"))?, identity);
    assert_eq!(std::fs::read(directory.join("rocky.sqlite"))?, database);
    let a = Device::new(Database::open(open(directory, SHELVED, None))?)?;
    create(
        &a,
        3,
        "items",
        json!({"title": "bread", "completed": false}),
    )
    .await;
    a.close().await;
    let stale = confirmation(Database::open_confirmed(
        open(directory, UNSHELVED, None),
        &required.report.digest,
    ));
    assert!(stale.mismatch);
    assert_eq!(stale.report.fields[0].rows, 2);
    let a = Device::new(Database::open_confirmed(
        open(directory, UNSHELVED, None),
        &stale.report.digest,
    )?)?;
    assert_eq!(generation(&a).await["current"]["schema"], 2);
    a.close().await;
    let (_, columns) = schema_of(directory)?;
    assert!(!columns.contains(&"category".into()));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_member_cannot_confirm_an_unsigned_removal_and_waits_for_an_issuer() -> Result<(), Failure>
{
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let (a_dir, b_dir) = (root.path().join("a"), root.path().join("b"));
    let a = Device::new(Database::create(open(&a_dir, SHELVED, network("a")), None)?)?;
    a.follow_main_migrating().await;
    let b = Device::new(Database::join(
        open(&b_dir, SHELVED, network("b")),
        &a.invite_as(json!({"kind": "writer", "scopes": ["items", "shelves"]}))
            .await,
    )?)?;
    b.close().await;
    let b = Device::new(Database::open_confirmed(
        open(&b_dir, UNSHELVED, None),
        "arbitrary",
    )?)?;
    let status = generation(&b).await;
    assert_eq!(status["current"]["schema"], 1);
    assert_eq!(status["application"], "ahead");
    assert_eq!(status["waiting_for_issuer"], true);
    assert!(
        b.database
            .session()
            .status()
            .await?
            .data
            .generation
            .waiting_for_issuer
    );
    b.close().await;
    a.close().await;
    assert!(schema_of(&b_dir)?.1.contains(&"category".into()));
    let a = Device::new(common::confirmed(open(&a_dir, UNSHELVED, network("a")))?)?;
    let b = Device::new(Database::open(open(&b_dir, UNSHELVED, network("b")))?)?;
    introduce(&a, &b).await;
    b.sync().await.expect("follow the issuer's removal");
    assert_eq!(generation(&b).await["current"]["schema"], 2);
    assert_eq!(generation(&b).await["waiting_for_issuer"], false);
    a.close().await;
    b.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_impact_report_counts_the_live_rows_of_all_local_partitions() -> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let a_dir = root.path().join("unique");
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let a = Device::new(Database::create(open(&a_dir, SHELVED, network("a")), None)?)?;
    a.follow_main_migrating().await;
    let peer_dir = root.path().join("peer");
    let peer = Device::new(Database::join(
        open(&peer_dir, SHELVED, network("peer")),
        &a.invite_as(json!({"kind":"writer", "scopes":["items", "shelves"]}))
            .await,
    )?)?;
    introduce(&a, &peer).await;
    create(
        &a,
        1,
        "items",
        json!({"title":"apples", "completed":false, "category":"same"}),
    )
    .await;
    create(
        &peer,
        2,
        "items",
        json!({"title":"bread", "completed":false, "category":"same"}),
    )
    .await;
    peer.sync().await.expect("conflicting writes sync");
    assert_eq!(a.titles().await.len(), 1);
    peer.close().await;
    a.close().await;
    let report = confirmation(Database::open(open(&a_dir, UNSHELVED, None))).report;
    assert_eq!(
        report.fields[0].rows, 1,
        "the later conflicting create dropped and left no row to count"
    );

    let b_dir = root.path().join("partitioned");
    let mut schema: Value = serde_json::from_str(NOTED)?;
    schema["tables"][0]["columns"]
        .as_array_mut()
        .ok_or("columns")?
        .push(json!({"name":"list", "type":"text", "not_null":true}));
    schema["tables"][0]["partition_by"] = json!(["list"]);
    schema["tables"][0]["primary_key"] = json!(["list", "key"]);
    let before = schema.to_string();
    let b = Device::new(Database::create(open(&b_dir, &before, None), None)?)?;
    b.follow_main_migrating().await;
    for (handle, list) in [(1, "first"), (2, "second")] {
        snap::ok(
            b.session
                .create_partition(rocky_sdk::PartitionTarget::Table {
                    table: "items".to_owned(),
                    values: vec![text(list)],
                }),
        )
        .await;

        snap::ok(b.session.execute("BEGIN", Vec::new())).await;
        let id =
            snap::ok(b.session.next_partition_id("items", vec![text(list)])).await["id"].clone();
        snap::ok(b.session.execute(
            &(snap::text(
                &("INSERT INTO items(list,key,title,completed,note,id) VALUES(?,?,?,?,?,?)"),
            )),
            vec![
                text(list),
                text("key"),
                text(list),
                rocky_sdk::SqlValue::Integer(0),
                text("old"),
                snap::row_id(&(id)),
            ],
        ))
        .await;
        snap::ok(b.session.commit([handle; 32])).await;
    }
    b.close().await;
    schema["version"] = json!(2);
    schema["tables"][0]["columns"]
        .as_array_mut()
        .ok_or("columns")?
        .retain(|column| column["name"] != "note");
    let report = confirmation(Database::open(open(&b_dir, &schema.to_string(), None))).report;
    assert_eq!(
        report.fields,
        [rocky_sdk::RemovalImpact {
            name: "items.note".into(),
            rows: 2
        }]
    );
    Ok(())
}
