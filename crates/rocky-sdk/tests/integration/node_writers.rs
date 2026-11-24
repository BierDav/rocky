use crate::support::snapshot as snap;
use common::{Device, Failure};
use rocky_sdk::{Network, transport::memory::Hub};
use std::sync::Arc;

use crate::common;

#[tokio::test]
async fn a_joined_node_keeps_its_key_as_writer_across_reopen_and_renewal() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let issuer = Device::create(
        &directory.path().join("issuer"),
        Some(Network::Owned(hub.connector("issuer"))),
    )
    .await?;
    let path = directory.path().join("writer");
    let open = || {
        crate::common::configuration(
            &path,
            common::parsed_schema(),
            None,
            common::prepared_identity(&path),
            Some(Network::Owned(hub.connector("writer"))),
        )
    };
    let writer = Device::new(open().join(&issuer.invite().await).await?)?;
    let before = snap::ok(snap::device(&writer.database)).await;
    let own = before["node"].as_str().ok_or("node missing")?.to_owned();
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
    let writer = Device::new(open().open().await?)?;
    assert_eq!(snap::ok(snap::device(&writer.database)).await["node"], own);
    common::introduce(&issuer, &writer).await;
    writer.database.renew().await?;
    assert_eq!(snap::ok(snap::device(&writer.database)).await["node"], own);
    writer.add(2, "after reopen").await;
    writer.close().await;
    issuer.close().await;
    Ok(())
}
