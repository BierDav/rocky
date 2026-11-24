use crate::common;
use crate::registry_revocation::fixture;
use crate::support::snapshot as snap;
use common::{Device, Failure};
use rocky_credentials::PrivateKey;
use rocky_sdk::{Affinity, Network, Schema, Table, transport::memory::Hub};
use serde_json::Value;
use serde_json::json;
use std::{path::Path, sync::Arc};
fn in_list(table: &str) -> Table {
    Table::replayed(table)
        .column(common::required("list", Affinity::Text))
        .column(common::required("key", Affinity::Text))
        .primary_key(["list", "key"])
        .partition_by(["list"])
        .partition_group("list")
}
fn siblings() -> Schema {
    common::build(
        Schema::builder("siblings", 1)
            .table(common::items())
            .table(in_list("parts"))
            .table(in_list("labels"))
            .table(in_list("attachments")),
    )
}
fn exposure() -> Schema {
    common::build(Schema::builder("exposure", 1).table(in_list("parts")))
}
fn open(root: &Path, hub: &Arc<Hub>, name: &str) -> rocky_sdk::Configuration {
    open_with(root, hub, name, &siblings())
}
fn open_with(root: &Path, hub: &Arc<Hub>, name: &str, schema: &Schema) -> rocky_sdk::Configuration {
    crate::common::configuration(
        root,
        schema,
        Some(fixture::keys(root)),
        None,
        Some(Network::Owned(hub.connector(name))),
    )
}
async fn join(
    root: &Path,
    hub: &Arc<Hub>,
    name: &str,
    a: &Device,
    access: rocky_sdk::Access,
    schema: &Schema,
) -> Result<Device, Failure> {
    let invitation = a.invite_as(access).await;
    let directory = root.join(name);
    let options = open_with(&directory, hub, name, schema);
    let device = Device::new(options.join(&invitation).await?)?;
    a.database.add_peer(&device.contact().await).await?;
    fixture::settle(&device).await;
    Ok(device)
}
async fn join_admin(
    root: &Path,
    hub: &Arc<Hub>,
    name: &str,
    a: &Device,
    schema: &Schema,
) -> Result<Device, Failure> {
    let device = join(root, hub, name, a, rocky_sdk::Access::Full, schema).await?;
    a.admit(&device).await;
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
            .database
            .partition_status(&rocky_sdk::Partition::new("list", values())),
    )
    .await
}
async fn create_partition(device: &Device) -> Value {
    snap::ok(
        device
            .database
            .create_partition(&rocky_sdk::Partition::new("list", values())),
    )
    .await;
    let status = snap::ok(
        device
            .database
            .partition_status(&rocky_sdk::Partition::new("list", values())),
    )
    .await;
    json!({ "epoch": status["current"] })
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
        snap::ok(issuer.database.set_sync(rocky_sdk::SyncMode::Off)).await;
        issuer.seal_and_compact().await;
    }
    for issuer in issuers {
        snap::ok(issuer.database.set_sync(rocky_sdk::SyncMode::Manual)).await;
    }
}

async fn sync_away_from_revoking_issuer(reader: &Device, revoking: &Device) -> Result<(), Failure> {
    snap::ok(revoking.database.set_sync(rocky_sdk::SyncMode::Off)).await;
    snap::ok(async {
        reader.database.renew().await?;
        snap::device(&reader.database).await
    })
    .await;
    snap::ok(
        reader
            .database
            .remove_peer(snap::public_key(&(revoking.node().await))),
    )
    .await;
    reader
        .sync()
        .await
        .map_err(|e| format!("reader sync: {e}"))?;
    snap::ok(revoking.database.set_sync(rocky_sdk::SyncMode::Manual)).await;
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
    let a = Device::new(open(&root.join("a"), &hub, "a").create().await?)?;
    let b = join_admin(root, &hub, "b", &a, &siblings()).await?;
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

#[tokio::test]
async fn concurrent_collection_rotations_open_both_rows_and_an_offline_writer_renews_once()
-> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let directory = root.join("a");
    let options = open_with(&directory, &hub, "a", &common::schema());
    let a = Device::new(options.create().await?)?;
    let b = join_admin(root, &hub, "b", &a, &common::schema()).await?;
    let offline = join(
        root,
        &hub,
        "offline",
        &a,
        rocky_sdk::Access::Full,
        &common::schema(),
    )
    .await?;
    offline.close().await;
    for device in [&a, &b] {
        snap::ok(device.database.remove(
            rocky_sdk::PersonId::new(PrivateKey::new().public_key()),
            rocky_sdk::Removal::Departed,
        ))
        .await;
    }
    let before_a = winner(root, "a", "items");
    let before_b = winner(root, "b", "items");
    assert_eq!(before_a.generation, 2);
    assert_eq!(before_b.generation, 2);
    assert_ne!(before_a, before_b);
    a.add(1, "a sibling").await;
    b.add(2, "b sibling").await;
    for device in [&a, &b, &a] {
        let may_wait_on_a_seal_rebootstrap_after_crossing_rotations = device.sync().await;
        drop(may_wait_on_a_seal_rebootstrap_after_crossing_rotations);
    }
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
    let options = open_with(&directory, &hub, "offline", &common::schema());
    let offline = Device::new(options.open().await?)?;
    snap::ok(async {
        offline.database.renew().await?;
        snap::device(&offline.database).await
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
async fn an_offline_write_on_the_losing_seal_line_reaches_every_device() -> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let directory = root.join("a");
    let options = open_with(&directory, &hub, "a", &common::schema());
    let a = Device::new(options.create().await?)?;
    let b = join_admin(root, &hub, "b", &a, &common::schema()).await?;
    let writer = join(
        root,
        &hub,
        "writer",
        &a,
        rocky_sdk::Access::Full,
        &common::schema(),
    )
    .await?;
    common::introduce(&writer, &b).await;
    a.add(1, "common").await;
    for device in [&b, &writer] {
        device.sync().await.map_err(|e| e.to_string())?;
    }
    snap::ok(a.database.set_sync(rocky_sdk::SyncMode::Off)).await;
    for device in [&a, &b] {
        snap::ok(device.database.remove(
            rocky_sdk::PersonId::new(PrivateKey::new().public_key()),
            rocky_sdk::Removal::Departed,
        ))
        .await;
    }
    assert_ne!(winner(root, "a", "items"), winner(root, "b", "items"));
    a.add(2, "a after its seal").await;
    b.add(3, "b after its seal").await;
    snap::ok(async {
        writer.database.renew().await?;
        snap::device(&writer.database).await
    })
    .await;
    writer.sync().await.map_err(|e| e.to_string())?;
    assert_eq!(
        winner(root, "writer", "items"),
        winner(root, "b", "items"),
        "the writer follows the line that later loses"
    );
    writer.add(4, "writer on the losing line").await;
    snap::ok(writer.database.set_sync(rocky_sdk::SyncMode::Off)).await;
    snap::ok(a.database.set_sync(rocky_sdk::SyncMode::Manual)).await;
    for device in [&a, &b] {
        let may_wait_on_a_seal_rebootstrap_after_crossing_rotations = device.sync().await;
        drop(may_wait_on_a_seal_rebootstrap_after_crossing_rotations);
    }
    snap::ok(writer.database.set_sync(rocky_sdk::SyncMode::Manual)).await;
    let every_row = [
        "a after its seal",
        "b after its seal",
        "common",
        "writer on the losing line",
    ];
    for _ in 0..3 {
        for device in [&writer, &a, &b] {
            let may_wait_on_a_seal_rebootstrap_after_crossing_rotations = device.sync().await;
            drop(may_wait_on_a_seal_rebootstrap_after_crossing_rotations);
        }
    }
    snap::ok(async {
        writer.database.renew().await?;
        snap::device(&writer.database).await
    })
    .await;
    let current = winner(root, "a", "items");
    for (name, device) in [("a", &a), ("b", &b), ("writer", &writer)] {
        let mut titles = device.titles().await;
        titles.sort();
        assert_eq!(titles, every_row, "{name} has every row");
        assert_eq!(winner(root, name, "items"), current, "{name} converged");
        assert_eq!(
            snap::ok(device.database.held_changes()).await["count"],
            0,
            "{name} holds nothing"
        );
    }
    for device in [&a, &b, &writer] {
        device.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn two_revoked_list_readers_open_the_opposite_sibling_but_not_the_superseding_rows()
-> Result<(), Failure> {
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
    let options = open_with(&directory, &hub, "a", &exposure());
    let a = Device::new(options.create().await?)?;
    create_partition(&a).await;
    write(&a, "before").await;
    let b = join_admin(root, &hub, "b", &a, &exposure()).await?;
    let reader = rocky_sdk::Access::Read;
    let carol = join(root, &hub, "carol", &a, reader.clone(), &exposure()).await?;
    let dave = join(root, &hub, "dave", &b, reader, &exposure()).await?;
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
                .database
                .remove_peer(snap::public_key(&(original.node().await))),
        )
        .await;
    }
    snap::ok(a.database.remove(
        rocky_sdk::PersonId::new(snap::public_key(&(carol.identity().await))),
        rocky_sdk::Removal::Departed,
    ))
    .await;
    snap::ok(b.database.remove(
        rocky_sdk::PersonId::new(snap::public_key(&(dave.identity().await))),
        rocky_sdk::Removal::Departed,
    ))
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
        assert!(
            device.purged().await,
            "a reader that learns its removal purges the rows it held instead of opening later ones"
        );
    }
    for device in [&a, &b, &carol, &dave] {
        device.close().await;
    }
    Ok(())
}
