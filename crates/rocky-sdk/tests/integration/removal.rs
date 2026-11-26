use crate::common;
use crate::support::snapshot as snap;
use common::{Device, Failure, build, introduce, items, required, shopping};
use rocky_sdk::{
    Affinity, Column, DeleteAction, ForeignKey, Index, Network, Schema, Table,
    transport::memory::Hub,
};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

fn shelved() -> Schema {
    build(
        shopping(1)
            .table(
                items()
                    .column(Column::new("category", Affinity::Text))
                    .column(Column::new("shelf", Affinity::Text))
                    .index(Index::new("items_category", ["category"]).unique())
                    .foreign_key(
                        ForeignKey::new(["shelf"], "shelves").on_delete(DeleteAction::SetNull),
                    ),
            )
            .table(
                Table::replayed("shelves")
                    .column(required("key", Affinity::Text))
                    .column(required("label", Affinity::Text))
                    .primary_key(["key"]),
            ),
    )
}
fn unshelved(version: u32) -> Schema {
    build(shopping(version).table(items()))
}
fn noted(version: u32) -> Schema {
    build(shopping(version).table(items().column(Column::new("note", Affinity::Text))))
}
fn partitioned_items() -> Table {
    items()
        .column(required("list", Affinity::Text))
        .partition_by(["list"])
        .primary_key(["list", "key"])
}

fn open(directory: &Path, schema: &Schema, network: Option<Network>) -> rocky_sdk::Configuration {
    crate::common::configuration(directory, schema, None, None, network)
}
async fn rows(device: &Device, sql: &str) -> Value {
    snap::ok(snap::read(&device.database, sql, Vec::new())).await["rows"].clone()
}
fn text(value: &str) -> rocky_sdk::SqlValue {
    rocky_sdk::SqlValue::Text(snap::text(&(value)))
}
async fn generation(device: &Device) -> Value {
    snap::ok(async {
        device
            .database
            .status()
            .await
            .map(|status| status.data().generation)
    })
    .await
}
async fn create(device: &Device, handle: u8, collection: &str, values: Value) -> String {
    let reply = snap::ok(snap::mutate(
        &device.database,
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
async fn write(device: &Device, _collection: &str, _handle: u8, sql: &str) {
    device
        .database
        .write(async |tx| tx.execute(sql, Vec::new()).await)
        .await
        .expect("write");
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
    let a = Device::new(open(&a_dir, &shelved(), network("a")).create().await?)?;
    let b = Device::new(
        open(&b_dir, &shelved(), network("b"))
            .join(&a.invite_as(rocky_sdk::Access::Full).await)
            .await?,
    )?;
    introduce(&a, &b).await;
    let shelf = create(&a, 1, "shelves", json!({"label": "pantry"})).await;
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
    let a = Device::new(common::confirmed(open(&a_dir, &unshelved(2), network("a"))).await?)?;
    assert_eq!(generation(&a).await["current"]["schema"], 2);
    assert_eq!(a.titles().await, ["bread", "green apples"]);
    b.close().await;
    let b = Device::new(open(&b_dir, &unshelved(2), network("b")).open().await?)?;
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
    let failed = snap::observe(snap::read(
        &b.database,
        "SELECT category FROM items",
        Vec::new(),
    ))
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
    let a = Device::new(
        open(&directory("a"), &noted(1), network("a"))
            .create()
            .await?,
    )?;
    let join = |name: &str, invitation: rocky_sdk::InvitationCode| {
        let configuration = open(&directory(name), &noted(1), network(name));
        async move { Device::new(configuration.join(&invitation).await?) }
    };
    let c = join("c", a.invite_as(rocky_sdk::Access::Full).await).await?;
    a.admit(&c).await;
    let d = join("d", a.invite().await).await?;
    let e = join("e", a.invite().await).await?;
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
        snap::ok(member.database.set_sync(rocky_sdk::SyncMode::Off)).await;
    }
    a.close().await;
    let removed =
        Device::new(common::confirmed(open(&directory("a"), &unshelved(2), None)).await?)?;
    removed.close().await;
    let a = Device::new(
        open(&directory("a"), &noted(3), network("a"))
            .open()
            .await?,
    )?;
    assert_eq!(generation(&a).await["current"]["schema"], 3);
    let notes = "SELECT title, note FROM items ORDER BY title";
    let empty = json!([[snap::cell(text("apples")), {"type": "null"}], [snap::cell(text("bread")), {"type": "null"}]]);
    assert_eq!(rows(&a, notes).await, empty);
    d.set(3, &apples, "note", json!("late")).await;

    snap::ok(c.database.set_sync(rocky_sdk::SyncMode::Manual)).await;
    c.sync().await.expect("sync");
    assert_eq!(
        generation(&c).await["current"]["schema"],
        3,
        "following main from A to C at once passes B's schema on the path"
    );

    assert_eq!(rows(&c, notes).await, empty);
    snap::ok(d.database.set_sync(rocky_sdk::SyncMode::Manual)).await;
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
    snap::ok(e.database.set_sync(rocky_sdk::SyncMode::Manual)).await;
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
    let a = Device::new(open(&a_dir, &shelved(), network("a")).create().await?)?;
    let b = Device::new(
        open(&b_dir, &shelved(), network("b"))
            .join(&a.invite_as(rocky_sdk::Access::Full).await)
            .await?,
    )?;
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
    b.database
        .write(async |tx| {
            tx.execute(
                "UPDATE items SET title='green apples' WHERE title='apples'",
                Vec::new(),
            )
            .await?;
            tx.execute(
                "INSERT INTO shelves(key,label) VALUES('cellar','cellar')",
                Vec::new(),
            )
            .await
        })
        .await?;
    assert_eq!(b.titles().await, ["bread", "green apples"]);

    a.close().await;
    let a = Device::new(common::confirmed(open(&a_dir, &unshelved(2), network("a"))).await?)?;
    b.close().await;
    let b = Device::new(open(&b_dir, &unshelved(2), network("b")).open().await?)?;
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
    result: Result<rocky_sdk::Database, rocky_sdk::Error>,
) -> rocky_sdk::RolloutConfirmationRequired {
    match result.err().expect("confirmation required").kind() {
        rocky_sdk::ErrorKind::RolloutConfirmation(required) => (**required).clone(),
        other => panic!("expected a structured removal report, found {other:?}"),
    }
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
    let a = Device::new(open(directory, &shelved(), None).create().await?)?;
    create(&a, 1, "shelves", json!({"label": "pantry"})).await;
    create(
        &a,
        2,
        "items",
        json!({"title": "apples", "completed": false, "category": "fruit"}),
    )
    .await;
    a.close().await;
    let identity = common::authority_bytes(directory, "identity")?;
    let database = std::fs::read(directory.join("rocky.sqlite"))?;
    let before = files(directory)?;
    let network = Arc::new(NeverStarts(std::sync::atomic::AtomicBool::new(false)));
    let required = confirmation(
        open(
            directory,
            &unshelved(2),
            Some(Network::Owned(network.clone())),
        )
        .open()
        .await,
    );
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
    assert_eq!(common::authority_bytes(directory, "identity")?, identity);
    assert_eq!(std::fs::read(directory.join("rocky.sqlite"))?, database);
    let changed = unshelved(3);
    let stale = confirmation(
        open(directory, &changed, None)
            .open_confirmed(&required.report.digest)
            .await,
    );
    assert!(stale.mismatch);
    assert_ne!(stale.report.digest, required.report.digest);
    assert_eq!(common::authority_bytes(directory, "identity")?, identity);
    assert_eq!(std::fs::read(directory.join("rocky.sqlite"))?, database);
    let a = Device::new(open(directory, &shelved(), None).open().await?)?;
    create(
        &a,
        3,
        "items",
        json!({"title": "bread", "completed": false}),
    )
    .await;
    a.close().await;
    let stale = confirmation(
        open(directory, &unshelved(2), None)
            .open_confirmed(&required.report.digest)
            .await,
    );
    assert!(stale.mismatch);
    assert_eq!(stale.report.fields[0].rows, 2);
    let a = Device::new(
        open(directory, &unshelved(2), None)
            .open_confirmed(&stale.report.digest)
            .await?,
    )?;
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
    let a = Device::new(open(&a_dir, &shelved(), network("a")).create().await?)?;
    let b = Device::new(
        open(&b_dir, &shelved(), network("b"))
            .join(&a.invite_as(rocky_sdk::Access::Full).await)
            .await?,
    )?;
    b.close().await;
    let b = Device::new(
        open(&b_dir, &unshelved(2), None)
            .open_confirmed("arbitrary")
            .await?,
    )?;
    let status = generation(&b).await;
    assert_eq!(status["current"]["schema"], 1);
    assert_eq!(status["application"], "ahead");
    assert_eq!(status["waiting_for_issuer"], true);
    assert!(
        b.database
            .status()
            .await?
            .data()
            .generation
            .waiting_for_issuer
    );
    b.close().await;
    a.close().await;
    assert!(schema_of(&b_dir)?.1.contains(&"category".into()));
    let a = Device::new(common::confirmed(open(&a_dir, &unshelved(2), network("a"))).await?)?;
    let b = Device::new(open(&b_dir, &unshelved(2), network("b")).open().await?)?;
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
    let a = Device::new(open(&a_dir, &shelved(), network("a")).create().await?)?;
    let peer_dir = root.path().join("peer");
    let peer = Device::new(
        open(&peer_dir, &shelved(), network("peer"))
            .join(&a.invite_as(rocky_sdk::Access::Full).await)
            .await?,
    )?;
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
    let report = confirmation(open(&a_dir, &unshelved(2), None).open().await).report;
    assert_eq!(
        report.fields[0].rows, 1,
        "the later conflicting create dropped and left no row to count"
    );

    let b_dir = root.path().join("partitioned");
    let before =
        build(shopping(1).table(partitioned_items().column(Column::new("note", Affinity::Text))));
    let b = Device::new(open(&b_dir, &before, None).create().await?)?;
    for (_handle, list) in [(1, "first"), (2, "second")] {
        snap::ok(
            b.database
                .create_partition(&rocky_sdk::Partition::new("items", vec![text(list)])),
        )
        .await;

        b.database
            .write(async |tx| {
                tx.execute(
                    "INSERT INTO items(list,key,title,completed,note) VALUES(?,?,?,?,?)",
                    vec![
                        text(list),
                        text("key"),
                        text(list),
                        rocky_sdk::SqlValue::Integer(0),
                        text("old"),
                    ],
                )
                .await
            })
            .await?;
    }
    b.close().await;
    let without_note = build(shopping(2).table(partitioned_items()));
    let report = confirmation(open(&b_dir, &without_note, None).open().await).report;
    assert_eq!(
        report.fields,
        [rocky_sdk::RemovalImpact {
            name: "items.note".into(),
            rows: 2
        }]
    );
    Ok(())
}
