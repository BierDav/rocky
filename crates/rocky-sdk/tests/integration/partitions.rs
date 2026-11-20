use crate::common;
use crate::support::snapshot as snap;
use rocky_sdk::{Database, Network, Open, transport::memory::Hub};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};
const SCHEMA: &str = r#"{"format":6,"application":"partitionsdk","version":1,"tables":[{"name":"items","class":"replayed","columns":[{"name":"list","type":"text","not_null":true},{"name":"key","type":"text","not_null":true},{"name":"amount","type":"integer","not_null":true}],"primary_key":["list","key"],"partition_by":["list"]}],"views":[{"name":"partial_items","create_sql":"CREATE VIEW partial_items AS SELECT * FROM items WHERE amount>=0"}]}"#;
fn open<'a>(directory: &'a Path, hub: &Arc<Hub>) -> Open<'a> {
    let name = directory.file_name().unwrap().to_str().unwrap();
    Open {
        identity: common::prepared_identity(directory),
        directory,
        schema: crate::support::snapshot::schema(SCHEMA),
        secrets: None,
        network: Some(Network::Owned(hub.connector(name))),
        coordination: None,
    }
}

fn values(name: &str) -> Vec<rocky_sdk::SqlValue> {
    vec![rocky_sdk::SqlValue::Text(snap::text(&(name)))]
}
#[tokio::test]
async fn public_partition_schema_keys_sql_identity_and_reopen() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("writer");
    let hub = Arc::new(Hub::default());
    let db = Database::create(open(&path, &hub), None).unwrap().database;
    let scope: Value = serde_json::to_value(db.scope()).unwrap();
    let session = db.session();
    for name in ["A", "B"] {
        snap::ok(session.create_partition(rocky_sdk::PartitionTarget::Table {
            table: "items".to_owned(),
            values: (values(name)).clone(),
        }))
        .await;
    }
    let mut ids = Vec::new();
    for (name, amount) in [("A", 10i64), ("B", 20i64)] {
        let mut tx = db.transaction().await.unwrap();
        let id = tx.next_id("items", values(name)).await.unwrap();
        ids.push(id);
        tx.execute(
            "INSERT INTO items(list,key,amount,id) VALUES(?,?,?,?)",
            vec![
                rocky_sdk::SqlValue::Text(name.to_owned()),
                rocky_sdk::SqlValue::Text("key".to_owned()),
                rocky_sdk::SqlValue::Integer(amount),
                id.sql(),
            ],
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
    assert_ne!(ids[0], ids[1], "one creation log per author and table");
    let driver = db
        .connect(rocky_sdk::ConnectionOptions {
            read_only: false,
            durability: rocky_sdk::Durability::Buffered,
            transaction_timeout: None,
            binding: None,
        })
        .await
        .unwrap();
    driver
        .execute(driver.statement(), "BEGIN".into(), vec![])
        .await
        .unwrap();
    let a_id = driver
        .execute(
            driver.statement(),
            "SELECT rocky_next_id('items', ?)".into(),
            vec![rocky_sdk::SqlValue::Text("A".into())],
        )
        .await
        .unwrap();
    let b_id = driver
        .execute(
            driver.statement(),
            "SELECT rocky_next_id('items', ?)".into(),
            vec![rocky_sdk::SqlValue::Text("B".into())],
        )
        .await
        .unwrap();
    assert_ne!(a_id.rows, b_id.rows);
    driver
        .execute(driver.statement(), "ROLLBACK".into(), vec![])
        .await
        .unwrap();

    let data = snap::ok(snap::read(
        &db,
        "SELECT SUM(amount) FROM partial_items",
        Vec::new(),
    ))
    .await;
    assert_eq!(data["rows"][0][0]["value"], "30");
    let address = snap::ok(session.partition_address("items", (values("A")).clone())).await;
    let parts = address["tuple"].as_array().unwrap();
    let selector = json!([[parts[0][0],{"Only":[parts[0][1]]}]]);
    let reader_path = root.path().join("reader");
    let device = common::node_keys(&reader_path, None).unwrap();
    let supplied=snap::ok(session.grant_partitions(device, serde_json::from_value::<Vec<rocky_sdk::credentials::PartitionGrant>>(json!([{"domain":scope,"table":address["scope"]["table"],"selector":selector,"rows":"All","rights":["Read"],"fields":[]}])).expect("typed fixture"), 1)).await;
    let reader = Database::install(
        open(&reader_path, &hub),
        serde_json::from_value(supplied["supplied"].clone()).unwrap(),
    )
    .unwrap()
    .database;
    let reader_session = reader.session();
    snap::ok(session.share_partition(rocky_sdk::PartitionTarget::Table { table: "items".to_owned(), values: (values("A")).clone() }, serde_json::from_value::<std::collections::BTreeSet<rocky_sdk::credentials::PublicKey>>(json!([device.node])).expect("typed fixture"), None)).await;
    let contact = snap::ok(session.contact_info()).await;
    snap::ok(async {
        let value = reader_session
            .add_peer(&(snap::text(&(contact["uri"]))))
            .await?;
        Ok(json!({"node":value}))
    })
    .await;
    snap::ok(reader_session.sync_once()).await;
    assert_eq!(
        snap::ok(
            reader_session.partition_status(rocky_sdk::PartitionTarget::Table {
                table: "items".to_owned(),
                values: (values("A")).clone()
            })
        )
        .await["epochs"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_ne!(
        snap::reply(
            reader_session.register_partition(
                rocky_sdk::PartitionTarget::Table {
                    table: "items".to_owned(),
                    values: (values("B")).clone()
                },
                rocky_sdk::PartitionSelection {
                    name: None,
                    omit: serde_json::from_value::<std::collections::BTreeSet<String>>(json!([]))
                        .expect("typed fixture")
                }
            )
        )
        .await["status"],
        "ok"
    );
    assert!(
        !snap::ok(async {
            let value = reader_session.partition_complete("items").await?;
            Ok(json!({"complete":value}))
        })
        .await["complete"]
            .as_bool()
            .unwrap()
    );
    snap::ok(async {
        reader_session
            .install_credentials(
                serde_json::from_value::<rocky_sdk::Supplied>(json!(supplied["supplied"]))
                    .expect("typed fixture"),
            )
            .await?;
        reader_session.info().await
    })
    .await;
    reader.close().await.unwrap();
    Database::open(open(&reader_path, &hub))
        .unwrap()
        .database
        .close()
        .await
        .unwrap();
    db.close().await.unwrap();
    let db = Database::open(open(&path, &hub)).unwrap().database;
    let session = db.session();
    let mut tx = db.transaction().await.unwrap();
    let id = tx.next_id("items", values("A")).await.unwrap();
    let first = ids[0];
    assert_eq!(
        (id.writer(), id.sequence()),
        (first.writer(), first.sequence() + 2)
    );
    tx.rollback().await.unwrap();
    snap::ok(session.create_partition(rocky_sdk::PartitionTarget::Table {
        table: "items".to_owned(),
        values: (values("C")).clone(),
    }))
    .await;
    let revoked =
        snap::ok(session.revoke_identity(snap::public_key(&(device.identity.to_string())))).await;
    assert_eq!(revoked["revocations"]["statements"], 1);
    let mut tx = db.transaction().await.unwrap();
    let id = tx.next_id("items", values("A")).await.unwrap();
    tx.execute(
        "INSERT INTO items(list,key,amount,id) VALUES('A','next',7,?)",
        vec![id.sql()],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    db.close().await.unwrap();
    Database::open(open(&path, &hub))
        .unwrap()
        .database
        .close()
        .await
        .unwrap();
}

#[tokio::test]
async fn own_id_partitions_continue_the_table_log() {
    let root = tempfile::tempdir().unwrap();
    let schema = r#"{"format":6,"application":"ownid","version":1,"tables":[{"name":"lists","class":"replayed","columns":[{"name":"title","type":"text","not_null":true}],"primary_key":["id"],"partition_by":["id"]}]}"#;
    let options = || Open {
        identity: None,
        directory: root.path(),
        schema: crate::support::snapshot::schema(schema),
        secrets: None,
        network: None,
        coordination: None,
    };
    let mut db = Database::create(options(), None).unwrap().database;
    let _scope: Value = serde_json::to_value(db.scope()).unwrap();
    let _session = db.session();
    let mut previous: Option<rocky_sdk::RowIdentity> = None;
    for index in 0..3 {
        if index == 2 {
            db.close().await.unwrap();
            db = Database::open(options()).unwrap().database;
        }
        let session = db.session();
        let prepared = snap::ok(session.next_partition("lists")).await;
        let mut tx = db.transaction().await.unwrap();
        let id = tx.next_id("lists", Vec::new()).await.unwrap();
        assert_eq!(id.to_string(), prepared["id"].as_str().unwrap());
        if let Some(previous) = previous {
            assert_eq!(
                id,
                rocky_sdk::RowIdentity::next(previous.writer(), previous.sequence()).unwrap()
            );
        }
        previous = Some(id);
        tx.execute(
            "INSERT INTO lists(id,title) VALUES(?,?)",
            vec![id.sql(), rocky_sdk::SqlValue::Text("list".to_owned())],
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
    db.close().await.unwrap();
}
