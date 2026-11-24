use crate::common;
use crate::support::snapshot as snap;
use rocky_sdk::{Network, transport::memory::Hub};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};
const SCHEMA: &str = r#"{"format":7,"application":"partitionsdk","version":1,"tables":[{"name":"items","class":"replayed","columns":[{"name":"list","type":"text","not_null":true},{"name":"key","type":"text","not_null":true},{"name":"amount","type":"integer","not_null":true}],"primary_key":["list","key"],"partition_by":["list"]}],"views":[{"name":"partial_items","create_sql":"CREATE VIEW partial_items AS SELECT * FROM items WHERE amount>=0"}],"roles":[{"name":"reader","tables":[{"table":"items","access":"read"}],"per_partition":["items"]}]}"#;
fn open(directory: &Path, hub: &Arc<Hub>) -> rocky_sdk::Configuration {
    let name = directory.file_name().unwrap().to_str().unwrap();
    crate::common::configuration(
        directory,
        crate::support::snapshot::schema(SCHEMA),
        None,
        common::prepared_identity(directory),
        Some(Network::Owned(hub.connector(name))),
    )
}

fn values(name: &str) -> Vec<rocky_sdk::SqlValue> {
    vec![rocky_sdk::SqlValue::Text(snap::text(&(name)))]
}
#[tokio::test]
async fn public_partition_schema_keys_sql_identity_and_reopen() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("writer");
    let hub = Arc::new(Hub::default());
    let db = open(&path, &hub).create().await.unwrap().database;
    for name in ["A", "B"] {
        snap::ok(db.create_partition(rocky_sdk::PartitionTarget::Table {
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
    let mut tx = db.transaction().await.unwrap();
    let mut next_id = async |list: &str| {
        tx.query(
            "SELECT rocky_next_id('items', ?)",
            vec![rocky_sdk::SqlValue::Text(list.into())],
        )
        .await
        .unwrap()
        .collect()
        .await
        .unwrap()[0]
            .get_at::<rocky_sdk::SqlValue>(0)
            .unwrap()
    };
    let a_id = next_id("A").await;
    let b_id = next_id("B").await;
    assert_ne!(a_id, b_id);
    tx.rollback().await.unwrap();

    let data = snap::ok(snap::read(
        &db,
        "SELECT SUM(amount) FROM partial_items",
        Vec::new(),
    ))
    .await;
    assert_eq!(data["rows"][0][0]["value"], "30");
    let reader_path = root.path().join("reader");
    let invitation = db
        .invite(rocky_sdk::Invite::new(rocky_sdk::Access::Role(
            rocky_sdk::RoleName::from_static("reader"),
        )))
        .await
        .unwrap();
    let reader = open(&reader_path, &hub)
        .join(&invitation.code)
        .await
        .unwrap()
        .database;
    let reader_identity: rocky_sdk::PublicKey = reader
        .status()
        .await
        .unwrap()
        .authority
        .principal
        .parse()
        .unwrap();
    snap::ok(db.grant_partition(
        reader_identity,
        rocky_sdk::PartitionTarget::Table {
            table: "items".to_owned(),
            values: values("A"),
        },
    ))
    .await;
    let contact = snap::ok(db.contact()).await;
    snap::ok(async {
        let value = reader.add_peer(&(snap::text(&(contact["uri"])))).await?;
        Ok::<_, rocky_sdk::Error>(json!({"node":value}))
    })
    .await;
    snap::ok(reader.sync_now()).await;
    assert_eq!(
        snap::ok(reader.partition_status(rocky_sdk::PartitionTarget::Table {
            table: "items".to_owned(),
            values: (values("A")).clone()
        }))
        .await["epochs"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_ne!(
        snap::reply(
            reader.register_partition(
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
            let value = reader.partition_complete("items").await?;
            Ok::<_, rocky_sdk::Error>(json!({"complete":value}))
        })
        .await["complete"]
            .as_bool()
            .unwrap()
    );
    snap::ok(async {
        reader.renew().await?;
        reader.info().await
    })
    .await;
    reader.close().await.unwrap();
    open(&reader_path, &hub)
        .open()
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
    db.close().await.unwrap();
    let db = open(&path, &hub).open().await.unwrap();
    let mut tx = db.transaction().await.unwrap();
    let id = tx.next_id("items", values("A")).await.unwrap();
    let first = ids[0];
    assert_eq!(
        (id.writer(), id.sequence()),
        (first.writer(), first.sequence() + 2)
    );
    tx.rollback().await.unwrap();
    snap::ok(db.create_partition(rocky_sdk::PartitionTarget::Table {
        table: "items".to_owned(),
        values: (values("C")).clone(),
    }))
    .await;
    let revoked =
        snap::ok(db.revoke_identity(snap::public_key(&(reader_identity.to_string())))).await;
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
    open(&path, &hub)
        .open()
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
}

#[tokio::test]
async fn own_id_partitions_continue_the_table_log() {
    let root = tempfile::tempdir().unwrap();
    let schema = r#"{"format":7,"application":"ownid","version":1,"tables":[{"name":"lists","class":"replayed","columns":[{"name":"title","type":"text","not_null":true}],"primary_key":["id"],"partition_by":["id"]}]}"#;
    let options = || {
        crate::common::configuration(
            root.path(),
            crate::support::snapshot::schema(schema),
            None,
            None,
            None,
        )
    };
    let mut db = options().create().await.unwrap().database;
    let _scope: Value = serde_json::to_value(db.scope()).unwrap();
    let mut previous: Option<rocky_sdk::RowIdentity> = None;
    for index in 0..3 {
        if index == 2 {
            db.close().await.unwrap();
            db = options().open().await.unwrap();
        }
        let prepared = snap::ok(db.next_partition("lists")).await;
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
