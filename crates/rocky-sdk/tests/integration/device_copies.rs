use crate::common::{self, Device, Failure};
use crate::support::snapshot as snap;
use rocky_sdk::{AlertKind, ErrorKind, Network, SqlValue, transport::memory::Hub};
use std::{path::Path, sync::Arc};

const READONLY: i32 = 8;

fn memory(hub: &Arc<Hub>, name: &str) -> Network {
    Network::Owned(hub.connector(name))
}

async fn try_add(device: &Device, key: &str) -> Result<(), rocky_sdk::Error> {
    let mut tx = device.database.transaction().await?;
    let id = tx.next_id("items", Vec::new()).await?;
    tx.execute(
        "INSERT INTO items(key,id,title,completed) VALUES(?,?,?,0)",
        vec![
            SqlValue::Text(key.into()),
            id.sql(),
            SqlValue::Text(key.into()),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn identities(device: &Device) -> Vec<String> {
    device
        .database
        .query("SELECT hex(id) FROM items ORDER BY id", Vec::new())
        .await
        .expect("read")
        .collect()
        .await
        .expect("collect")
        .into_iter()
        .map(|row| {
            row.get_at::<SqlValue>(0)
                .expect("id")
                .to_text()
                .expect("hex id")
        })
        .collect()
}

fn copy_replica(from: &Path, to: &Path) -> Result<(), Failure> {
    std::fs::create_dir_all(to)?;
    for name in ["rocky.sqlite", "rocky.sqlite-wal", "rocky.sqlite-shm"] {
        let _ = std::fs::remove_file(to.join(name));
        if from.join(name).exists() {
            std::fs::copy(from.join(name), to.join(name))?;
        }
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wiped_worker_rejoins_as_a_new_device_and_catches_up_before_it_authors()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let a = Device::create(&root.path().join("a"), Some(memory(&hub, "a"))).await?;
    let worker = root.path().join("worker");
    let w = Device::join(&worker, &a.invite().await, memory(&hub, "w")).await?;
    w.sync().await.expect("join sync");
    w.add(1, "first").await;
    w.add(2, "second").await;
    w.sync().await.expect("sync");
    assert_eq!(a.titles().await.len(), 2);
    let before = identities(&a).await;
    let node = w.node().await;
    w.close().await;
    drop(w);
    std::fs::remove_dir_all(&worker)?;

    let w = Device::join(&worker, &a.invite().await, memory(&hub, "w2")).await?;
    assert_ne!(w.node().await, node);
    w.sync().await.expect("catch up");
    assert_eq!(identities(&w).await, before);
    try_add(&w, "third").await.expect("caught-up lane authors");
    w.sync().await.expect("sync");
    let after = identities(&a).await;
    assert_eq!(after.len(), 3, "{after:?}");
    assert!(before.iter().all(|id| after.contains(id)));
    assert_eq!(after, identities(&w).await);
    w.close().await;
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restored_copy_never_authors_and_rejoins_as_a_new_device() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let a = Device::create(&root.path().join("a"), Some(memory(&hub, "a"))).await?;
    let (device, backup) = (root.path().join("b"), root.path().join("backup"));
    let b = Device::join(&device, &a.invite().await, memory(&hub, "b")).await?;
    b.sync().await.expect("join sync");
    b.add(1, "before the backup").await;
    b.sync().await.expect("sync");
    let old_identity = b.identity().await;
    b.close().await;
    drop(b);
    copy_replica(&device, &backup)?;

    let b = Device::open(&device, Some(memory(&hub, "b"))).await?;
    b.sync().await.expect("sync");
    b.add(2, "after the backup").await;
    b.sync().await.expect("sync");
    b.close().await;
    drop(b);
    copy_replica(&backup, &device)?;

    let restored = Device::open(&device, Some(memory(&hub, "b"))).await?;
    restored
        .sync()
        .await
        .expect("a superseded copy still syncs");
    assert_eq!(
        restored.titles().await,
        ["after the backup", "before the backup"]
    );
    let refused = try_add(&restored, "from the restored copy")
        .await
        .expect_err("superseded");
    assert!(
        matches!(refused.kind(), ErrorKind::Sql(failure) if failure.code == READONLY),
        "{refused:?}"
    );
    assert!(!refused.retryable());
    assert!(refused.detail().contains("re-invite it as a new device"));
    let status = restored.database.status().await?;
    let restored_node = restored.node().await;
    assert!(
        status.alerts.iter().any(|alert| matches!(
            &alert.kind,
            AlertKind::StoreSuperseded { node, .. } if *node == restored_node
        )),
        "{:?}",
        status.alerts
    );
    restored.close().await;
    drop(restored);

    a.database
        .remove(
            rocky_sdk::PersonId::new(snap::public_key(&old_identity)),
            rocky_sdk::Removal::Departed,
        )
        .await?;
    let d = Device::join(&root.path().join("d"), &a.invite().await, memory(&hub, "d")).await?;
    d.sync().await.expect("join sync");
    assert_ne!(d.node().await, restored_node);
    d.add(3, "from the new device").await;
    assert!(common::reconciled(&d.sync().await.expect("sync")));
    assert_eq!(
        a.titles().await,
        [
            "after the backup",
            "before the backup",
            "from the new device"
        ]
    );
    assert_eq!(a.items().await, d.items().await);
    d.close().await;
    a.close().await;
    Ok(())
}
