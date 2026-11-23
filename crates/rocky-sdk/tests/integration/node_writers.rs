use crate::support::snapshot as snap;
use common::{Device, Failure};
use rocky_sdk::{Database, Network, Open, transport::memory::Hub};
use std::sync::Arc;

use crate::common;

#[tokio::test]
async fn a_joined_node_keeps_its_key_as_writer_across_reopen_and_renewal() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let issuer = Device::create(
        &directory.path().join("issuer"),
        Some(Network::Owned(hub.connector("issuer"))),
    )?;
    let path = directory.path().join("writer");
    let open = || Open {
        identity: common::prepared_identity(&path),
        directory: &path,
        schema: crate::support::snapshot::schema(common::SCHEMA),
        secrets: None,
        network: Some(Network::Owned(hub.connector("writer"))),
    };
    let writer = Device::new(Database::join(open(), &issuer.invite().await)?)?;
    let before = snap::ok(writer.session.info()).await;
    let own = before["writer"]
        .as_str()
        .ok_or("writer missing")?
        .to_owned();
    assert_eq!(before["node"].as_str(), Some(own.as_str()));
    writer.add(1, "first").await;
    let mut tx = writer.database.transaction().await?;
    let id = tx.next_id("items", Vec::new()).await?.to_string();
    assert_eq!(
        (id.len(), &id[..64]),
        (80, own.as_str()),
        "a row identity is the writer's key followed by its sequence"
    );
    tx.rollback().await?;
    writer.close().await;
    let writer = Device::new(Database::open(open())?)?;
    assert_eq!(snap::ok(writer.session.info()).await["writer"], own);
    common::introduce(&issuer, &writer).await;
    writer.session.renew().await?;
    assert_eq!(snap::ok(writer.session.info()).await["writer"], own);
    writer.add(2, "after reopen").await;
    writer.close().await;
    issuer.close().await;
    Ok(())
}
