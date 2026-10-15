//! Durable principal ranges and local row creation under numbered node keys.
use common::{Device, Failure};
use rocky_sdk::{Database, Open};
use serde_json::json;

mod common;

#[tokio::test]
async fn supplied_numbered_node_writes_offline_reopens_and_keeps_its_granted_range()
-> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let issuer = Device::create(&directory.path().join("issuer"), None)?;
    let path = directory.path().join("writer");
    let keys = Database::device(&path, None)?;
    let profile = json!({"kind":"writer","scopes":["items"]});
    let grant = json!({"type":"grant","device":keys,"profile":profile,"authors":2});
    let supplied = issuer.ok(grant.clone()).await["supplied"].clone();
    let open = || Open {
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
    let before = writer.ok(json!({"type":"session"})).await;
    let author = before["author"].as_u64().ok_or("author missing")?;
    assert_eq!(author, 2);
    writer.add(1, "first").await;
    writer.ok(json!({"type":"sql","sql":"BEGIN"})).await;
    let note = writer.ok(json!({"type":"next_id","table":"items"})).await;
    assert_eq!(
        note["id"].as_str().ok_or("id")?.parse::<u64>()? >> 32,
        author
    );
    writer.ok(json!({"type":"sql","sql":"ROLLBACK"})).await;
    writer.close().await;
    let writer = Device::new(Database::open(open())?)?;
    assert_eq!(writer.ok(json!({"type":"session"})).await["author"], author);
    let supplied = issuer.ok(grant).await["supplied"].clone();
    writer
        .ok(json!({"type":"install","supplied":supplied}))
        .await;
    assert_eq!(writer.ok(json!({"type":"session"})).await["author"], author);
    writer.add(2, "after reopen").await;
    writer.close().await;
    issuer.close().await;
    Ok(())
}
