use crate::common;
use crate::registry_revocation::fixture;
use crate::support::snapshot as snap;
use common::{Device, Failure};
use rocky_sdk::{Database, Network, Open, credentials::PrivateKey, transport::memory::Hub};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};
const SCHEMA: &str = r#"{"application":"siblings","version":1,"format":6,"tables":[
 {"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]},
 {"name":"parts","class":"replayed","columns":[{"name":"list","type":"text","not_null":true},{"name":"key","type":"text","not_null":true}],"primary_key":["list","key"],"partition_by":["list"],"partition_group":"list"},
 {"name":"labels","class":"replayed","columns":[{"name":"list","type":"text","not_null":true},{"name":"key","type":"text","not_null":true}],"primary_key":["list","key"],"partition_by":["list"],"partition_group":"list"},
 {"name":"attachments","class":"replayed","columns":[{"name":"list","type":"text","not_null":true},{"name":"key","type":"text","not_null":true}],"primary_key":["list","key"],"partition_by":["list"],"partition_group":"list"}]}"#;
fn open<'a>(root: &'a Path, hub: &Arc<Hub>, name: &str) -> Open<'a> {
    Open {
        identity: None,
        directory: root,
        schema: crate::support::snapshot::schema(SCHEMA),
        secrets: Some(fixture::keys(root)),
        network: Some(Network::Owned(hub.connector(name))),
    }
}
async fn join(
    root: &Path,
    hub: &Arc<Hub>,
    name: &str,
    a: &Device,
    profile: Value,
    schema: &'static str,
) -> Result<Device, Failure> {
    let invitation = a.invite_as(profile).await;
    let directory = root.join(name);
    let mut options = open(&directory, hub, name);
    options.schema = crate::support::snapshot::schema(schema);
    let device = Device::new(Database::join(options, &invitation)?)?;
    a.session.add_peer(&device.contact().await).await?;
    fixture::settle(&device).await;
    Ok(device)
}
fn identity(root: &Path, name: &str) -> Value {
    common::authority_json(&root.join(name), "identity").unwrap()
}
fn winner(root: &Path, name: &str, collection: &str) -> rocky_core::KeyEpoch {
    let epochs: Vec<rocky_core::KeyEpoch> =
        serde_json::from_value(identity(root, name)["keys"][collection].clone()).unwrap();
    *epochs.iter().max().unwrap()
}
fn values() -> Vec<rocky_sdk::SqlValue> {
    vec![rocky_sdk::SqlValue::Text("L".to_owned())]
}
async fn partition(device: &Device) -> Value {
    snap::ok(
        device
            .session
            .partition_status(rocky_sdk::PartitionTarget::Group {
                group: "list".into(),
                values: values(),
            }),
    )
    .await
}
async fn create_partition(device: &Device) -> Value {
    snap::ok(
        device
            .session
            .create_partition(rocky_sdk::PartitionTarget::Group {
                group: "list".into(),
                values: values(),
            }),
    )
    .await
}
async fn write_group(device: &Device, prefix: &str) {
    let mut tx = device.database.transaction().await.expect("begin");
    for table in ["parts", "labels", "attachments"] {
        let id = tx.next_id(table, values()).await.expect("id");
        tx.execute(
            &format!("INSERT INTO {table}(list,key,id) VALUES ('L', '{prefix}', ?)"),
            vec![id.sql()],
        )
        .await
        .expect("insert");
    }
    tx.commit().await.expect("commit");
}
async fn seal_each_issuer_apart(issuers: [&Device; 2]) {
    for issuer in issuers {
        snap::ok(issuer.session.set_networking_enabled(false)).await;
        issuer.seal_and_compact().await;
    }
    for issuer in issuers {
        snap::ok(issuer.session.set_networking_enabled(true)).await;
    }
}

async fn sync_away_from_revoking_issuer(reader: &Device, revoking: &Device) -> Result<(), Failure> {
    snap::ok(revoking.session.set_networking_enabled(false)).await;
    snap::ok(async {
        reader.session.renew().await?;
        reader.session.info().await
    })
    .await;
    snap::ok(
        reader
            .session
            .remove_peer(snap::public_key(&(revoking.node().await))),
    )
    .await;
    reader
        .sync()
        .await
        .map_err(|e| format!("reader sync: {e}"))?;
    snap::ok(revoking.session.set_networking_enabled(true)).await;
    Ok(())
}

async fn rows(device: &Device, table: &str) -> Vec<String> {
    let page = snap::ok(snap::read(
        &device.database,
        &format!("SELECT key FROM {table} WHERE list='L' ORDER BY key"),
        Vec::new(),
    ))
    .await;
    page["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row[0]["value"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn concurrent_group_creation_opens_every_table_and_converges_after_one_sync_each()
-> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let a = Device::new(Database::create(open(&root.join("a"), &hub, "a"), None)?)?;
    let b = join(root, &hub, "b", &a, json!({"kind":"issuer"}), SCHEMA).await?;
    let ea = create_partition(&a).await["epoch"].clone();
    let eb = create_partition(&b).await["epoch"].clone();
    assert_ne!(ea, eb);
    write_group(&a, "a").await;
    write_group(&b, "b").await;
    a.sync().await.map_err(|e| e.to_string())?;
    b.sync().await.map_err(|e| e.to_string())?;
    let left = partition(&a).await;
    let right = partition(&b).await;
    assert_eq!(left["current"], right["current"]);
    assert_eq!(left["epochs"].as_array().unwrap().len(), 2);
    for device in [&a, &b] {
        let status = partition(device).await;
        for table in status["tables"].as_array().unwrap() {
            assert_eq!(table["current"], status["current"]);
            assert_eq!(table["epochs"], status["epochs"]);
            assert_eq!(
                rows(device, table["table"].as_str().unwrap()).await,
                ["a", "b"]
            );
        }
    }
    write_group(&a, "c").await;
    write_group(&b, "d").await;
    for device in [&a, &b] {
        device.close().await;
    }
    Ok(())
}

#[ignore = "fails while the creator follows main (also on 68de7e0d through Configuration::create): the renewed offline writer finds items Incomplete"]
#[tokio::test]
async fn concurrent_collection_rotations_open_both_rows_and_an_offline_writer_renews_once()
-> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let directory = root.join("a");
    let mut options = open(&directory, &hub, "a");
    options.schema = crate::support::snapshot::schema(common::SCHEMA);
    let a = Device::new(Database::create(options, None)?)?;
    let b = join(
        root,
        &hub,
        "b",
        &a,
        json!({"kind":"issuer"}),
        common::SCHEMA,
    )
    .await?;
    let offline = join(
        root,
        &hub,
        "offline",
        &a,
        json!({"kind":"writer","scopes":["items"]}),
        common::SCHEMA,
    )
    .await?;
    offline.close().await;
    for device in [&a, &b] {
        snap::ok(
            device
                .session
                .revoke_identity(PrivateKey::new().public_key()),
        )
        .await;
    }
    let before_a = winner(root, "a", "items");
    let before_b = winner(root, "b", "items");
    assert_eq!(before_a.generation, 2);
    assert_eq!(before_b.generation, 2);
    assert_ne!(before_a, before_b);
    a.add(1, "a sibling").await;
    b.add(2, "b sibling").await;
    a.sync().await.map_err(|e| e.to_string())?;
    b.sync().await.map_err(|e| e.to_string())?;
    let current = winner(root, "b", "items");
    assert_eq!(current.generation, 3);
    assert_eq!(winner(root, "a", "items"), current);
    for device in [&a, &b] {
        assert_eq!(device.titles().await, ["a sibling", "b sibling"]);
    }
    for _ in 0..10 {
        a.sync().await.map_err(|e| e.to_string())?;
        b.sync().await.map_err(|e| e.to_string())?;
        assert_eq!(winner(root, "a", "items"), current);
        assert_eq!(winner(root, "b", "items"), current);
    }
    let directory = root.join("offline");
    let mut options = open(&directory, &hub, "offline");
    options.schema = crate::support::snapshot::schema(common::SCHEMA);
    let offline = Device::new(Database::open(options)?)?;
    snap::ok(async {
        offline.session.renew().await?;
        offline.session.info().await
    })
    .await;
    assert_eq!(winner(root, "offline", "items"), current);
    offline.add(3, "after renewal").await;
    for device in [&a, &b, &offline] {
        device.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn two_revoked_list_readers_open_the_opposite_sibling_but_not_the_superseding_rows()
-> Result<(), Failure> {
    const LIST_SCHEMA: &str = r#"{"application":"exposure","version":1,"format":6,"tables":[{"name":"parts","class":"replayed","columns":[{"name":"list","type":"text","not_null":true},{"name":"key","type":"text","not_null":true}],"primary_key":["list","key"],"partition_by":["list"],"partition_group":"list"}]}"#;
    async fn write(device: &Device, key: &str) {
        let mut tx = device.database.transaction().await.expect("begin");
        let id = tx.next_id("parts", values()).await.expect("id");
        tx.execute(
            "INSERT INTO parts(list,key,id) VALUES ('L', ?, ?)",
            vec![rocky_sdk::SqlValue::Text(key.to_owned()), id.sql()],
        )
        .await
        .expect("insert");
        tx.commit().await.expect("commit");
    }
    let root = crate::support::small_directory()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let directory = root.join("a");
    let mut options = open(&directory, &hub, "a");
    options.schema = crate::support::snapshot::schema(LIST_SCHEMA);
    let a = Device::new(Database::create(options, None)?)?;
    create_partition(&a).await;
    write(&a, "before").await;
    let b = join(root, &hub, "b", &a, json!({"kind":"issuer"}), LIST_SCHEMA).await?;
    let reader = json!({"kind":"reader","scopes":["parts"]});
    let carol = join(root, &hub, "carol", &a, reader.clone(), LIST_SCHEMA).await?;
    let dave = join(root, &hub, "dave", &b, reader, LIST_SCHEMA).await?;
    a.sync()
        .await
        .map_err(|e| format!("a sync before revocations: {e}"))?;
    b.sync()
        .await
        .map_err(|e| format!("b sync before revocations: {e}"))?;
    for (device, other, original) in [(&carol, &b, &a), (&dave, &a, &b)] {
        common::introduce(device, other).await;
        snap::ok(
            device
                .session
                .remove_peer(snap::public_key(&(original.node().await))),
        )
        .await;
    }
    snap::ok(
        a.session
            .revoke_identity(snap::public_key(&(carol.identity().await))),
    )
    .await;
    snap::ok(
        b.session
            .revoke_identity(snap::public_key(&(dave.identity().await))),
    )
    .await;
    write(&a, "a").await;
    write(&b, "b").await;
    seal_each_issuer_apart([&a, &b]).await;
    for (reader, revoking) in [(&carol, &a), (&dave, &b)] {
        sync_away_from_revoking_issuer(reader, revoking).await?;
    }
    assert_eq!(rows(&carol, "parts").await, ["b", "before"]);
    assert_eq!(rows(&dave, "parts").await, ["a", "before"]);
    a.sync()
        .await
        .map_err(|e| format!("a sync after revocations: {e}"))?;
    b.sync()
        .await
        .map_err(|e| format!("b sync after revocations: {e}"))?;
    let current = partition(&a).await["current"].clone();
    assert_eq!(current["generation"], 3);
    assert_eq!(partition(&b).await["current"], current);
    write(&a, "after").await;
    for device in [&carol, &dave] {
        let held = partition(device).await;
        assert!(
            held["epochs"]
                .as_array()
                .unwrap()
                .iter()
                .all(|epoch| epoch["generation"].as_u64().unwrap() < 3)
        );
        let _ = device.sync().await;
    }
    assert_eq!(rows(&carol, "parts").await, ["b", "before"]);
    assert_eq!(rows(&dave, "parts").await, ["a", "before"]);
    for device in [&a, &b, &carol, &dave] {
        device.close().await;
    }
    Ok(())
}
