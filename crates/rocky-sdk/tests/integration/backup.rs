//! Signed backup archives restored into a fresh signing identity through
//! the blocking constructor; `backup::tests` covers `Configuration`.
use crate::common;
use crate::support::snapshot as snap;
use common::{Device, Failure, SCHEMA, introduce};
use rocky_sdk::{Database, Network, Open, transport::memory::Hub};
use serde_json::json;
use std::{path::Path, sync::Arc};

fn open(directory: &Path, network: Option<Network>) -> Open<'_> {
    Open {
        identity: None,
        directory,
        schema: SCHEMA,
        secrets: None,
        network,
        coordination: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_backup_restores_into_a_fresh_identity_without_forking_or_resending()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let a = Device::create(&root.path().join("a"), network("a"))?;
    let b = Device::join(
        &root.path().join("b"),
        &a.invite().await,
        Network::Owned(hub.connector("b")),
    )?;
    introduce(&a, &b).await;
    b.add(1, "apples").await;
    b.sync().await.expect("sync");
    // Bread never left the device that is about to be lost.
    b.add(2, "bread").await;
    let lost = b.node().await;
    // A retired cache beside the database is never copied into a current archive.
    std::fs::write(
        root.path().join("b/coordinator.evidence"),
        b"retired receipt cache",
    )?;
    let archive = root.path().join("b.backup");
    let (first, second) = tokio::join!(b.database.backup(&archive), b.database.backup(&archive));
    assert_ne!(first.is_ok(), second.is_ok(), "one archive writer wins");
    let inventory = first.or(second)?;
    assert_eq!(inventory.node.to_hex(), lost);
    let names: Vec<&str> = inventory
        .files
        .iter()
        .map(|file| file.name.as_str())
        .collect();
    assert_eq!(names, ["identity.json", "rocky.sqlite"]);
    assert!(
        b.database.backup(&archive).await.is_err(),
        "never overwritten"
    );
    b.close().await;

    let retired = root.path().join("retired.backup");
    let mut retired_bytes = std::fs::read(&archive)?;
    let old_magic = b"rocky.backup.v3\n";
    retired_bytes[..old_magic.len()].copy_from_slice(old_magic);
    std::fs::write(&retired, retired_bytes)?;
    let error = Database::restore(
        open(&root.path().join("old"), network("old")),
        &retired,
        &a.invite().await,
    )
    .err()
    .expect("retired archive")
    .to_string();
    assert!(error.contains("not a Rocky backup archive"), "{error}");

    // A changed byte, or an invitation of another domain, is refused.
    let mut bytes = std::fs::read(&archive)?;
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    let tampered = root.path().join("tampered.backup");
    std::fs::write(&tampered, bytes)?;
    let invitation = a.invite().await;
    assert!(
        Database::restore(
            open(&root.path().join("t"), network("t")),
            &tampered,
            &invitation
        )
        .is_err()
    );
    let other = Device::create(&root.path().join("other"), network("other"))?;
    let foreign = other.invite().await;
    let refused = Database::restore(
        open(&root.path().join("f"), network("f")),
        &archive,
        &foreign,
    );
    let error = refused.err().expect("another domain").to_string();
    assert!(error.contains("another domain"), "{error}");
    other.close().await;

    let c_dir = root.path().join("c");
    let c = Device::new(Database::restore(
        open(&c_dir, network("c")),
        &archive,
        &a.invite().await,
    )?)?;
    assert_ne!(c.node().await, lost, "a fresh signing identity");
    assert_eq!(c.titles().await, ["apples", "bread"]);
    // The lost device's handle still names its one operation.
    let replayed = snap::ok(snap::mutate(
        &c.session,
        vec![2; 32],
        snap::Action::Create {
            table: "items".to_owned(),
            values: vec![
                ("title".to_owned(), snap::field(json!("bread"))),
                ("completed".to_owned(), snap::field(json!(false))),
            ],
        },
        None,
        Some(snap::text(&(lost))),
    ))
    .await;
    assert_eq!(replayed["replayed"], true);
    assert_eq!(c.titles().await, ["apples", "bread"]);
    // Its unsent edit is delivered as the lost device signed it; new edits
    // are the new identity's own.
    c.sync().await.expect("sync");
    assert_eq!(a.titles().await, ["apples", "bread"]);
    c.add(3, "cheese").await;
    c.sync().await.expect("sync");
    assert_eq!(a.titles().await, ["apples", "bread", "cheese"]);
    c.close().await;
    // The directory now holds a domain: the archive is not restored again.
    assert!(Database::restore(open(&c_dir, network("c")), &archive, &a.invite().await).is_err());
    let c = Device::new(Database::open(open(&c_dir, None))?)?;
    assert_eq!(c.titles().await, ["apples", "bread", "cheese"]);
    c.close().await;
    a.close().await;
    Ok(())
}
