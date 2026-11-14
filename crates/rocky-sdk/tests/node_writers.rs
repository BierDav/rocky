//! A node writes under its own key: offline, after reopening and after its
//! credentials are supplied again, its rows keep one writer identity.
// The SDK's blocking constructors, native transports and paused clocks.
#![cfg(not(target_family = "wasm"))]
use common::identity::snapshot as snap;
use common::{Device, Failure};
use rocky_sdk::{Database, Open};
use serde_json::json;

mod common;

#[tokio::test]
async fn a_supplied_node_keeps_its_key_as_writer_across_reopen_and_renewal() -> Result<(), Failure>
{
    let directory = tempfile::tempdir()?;
    let issuer = Device::create(&directory.path().join("issuer"), None)?;
    let path = directory.path().join("writer");
    let keys = common::node_keys(&path, None)?;
    let profile = json!({"kind":"writer","scopes":["items"]});
    let grant = || {
        issuer.session.grant(
            keys,
            serde_json::from_value::<rocky_sdk::GrantProfile>(json!(profile))
                .expect("typed fixture"),
            rocky_sdk::GrantTerms {
                ttl: None,
                delegations: None,
            },
        )
    };
    let supplied = snap::ok(grant()).await["supplied"].clone();
    let open = || Open {
        identity: common::prepared_identity(&path),
        directory: &path,
        schema: common::SCHEMA,
        secrets: None,
        network: None,
        coordination: None,
    };
    let writer = Device::new(Database::install(
        open(),
        serde_json::from_value(supplied)?,
    )?)?;
    let before = snap::ok(writer.session.info()).await;
    let own = before["writer"]
        .as_str()
        .ok_or("writer missing")?
        .to_owned();
    assert_eq!(before["node"].as_str(), Some(own.as_str()));
    writer.add(1, "first").await;
    snap::ok(writer.session.execute("BEGIN", Vec::new())).await;
    let note = snap::ok(writer.session.next_id("items")).await;
    // A row identity is the writer's key followed by its sequence.
    let id = note["id"].as_str().ok_or("id")?;
    assert_eq!((id.len(), &id[..64]), (80, own.as_str()));
    snap::ok(writer.session.execute("ROLLBACK", Vec::new())).await;
    writer.close().await;
    let writer = Device::new(Database::open(open())?)?;
    assert_eq!(snap::ok(writer.session.info()).await["writer"], own);
    let supplied = snap::ok(grant()).await["supplied"].clone();
    snap::ok(async {
        writer
            .session
            .install_credentials(
                serde_json::from_value::<rocky_sdk::Supplied>(json!(supplied))
                    .expect("typed fixture"),
            )
            .await?;
        writer.session.info().await
    })
    .await;
    assert_eq!(snap::ok(writer.session.info()).await["writer"], own);
    writer.add(2, "after reopen").await;
    writer.close().await;
    issuer.close().await;
    Ok(())
}
