use super::*;
use rocky_sdk::{Database, Open};
use std::path::Path;

fn schema(version: u32, migration: bool) -> String {
    let mut schema: Value = serde_json::from_str(common::SCHEMA).unwrap();
    schema["version"] = json!(version);
    schema["tables"][0]["columns"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":"note", "type":"text"}));
    if migration {
        schema["migrations"] = json!([{"version":version,"statements":["UPDATE items SET note=title || '-migrated'"]}]);
    }
    schema.to_string()
}
fn open<'a>(directory: &'a Path, schema: &'a str) -> Open<'a> {
    Open {
        directory,
        schema,
        identity: None,
        secrets: None,
        network: None,
        coordination: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn additive_publication_requires_following_and_old_app_opens_behind() -> Result<(), Failure> {
    let root = common::identity::small_directory()?;
    let a = Device::create(root.path(), None)?;
    a.add(1, "apples").await;
    a.close().await;
    let declared = schema(2, false);
    let a = Device::new(Database::open(open(root.path(), &declared))?)?;
    let status = a.ok(json!({"type":"generation"})).await;
    assert_eq!(status["application"], "ahead");
    assert_eq!(status["reason"], "follow_required");
    let before = main(&a).await["tip"].clone();
    a.close().await;
    let a = Device::new(Database::open(open(root.path(), &declared))?)?;
    let status = a.ok(json!({"type":"generation"})).await;
    assert_eq!(status["application"], "current", "{status}");
    assert_eq!(status["current"]["schema"], 2);
    assert_ne!(ok(&a, "status", json!({})).await["tip"], before);
    assert_eq!(a.titles().await, ["apples"]);
    a.close().await;
    let a = Device::open(root.path(), None)?;
    let status = a.ok(json!({"type":"generation"})).await;
    assert_eq!(status["application"], "behind", "{status}");
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn migration_closes_opens_a_new_line_and_commits_its_program_once() -> Result<(), Failure> {
    let root = common::identity::small_directory()?;
    let a = Device::create(root.path(), None)?;
    main(&a).await;
    ok(&a, "genesis_minimum", json!({"minimum_in_sync":1})).await;
    a.add(1, "apples").await;
    a.close().await;
    let declared = schema(2, true);
    for _ in 0..2 {
        let a = Device::new(Database::open(open(root.path(), &declared))?)?;
        let status = a.ok(json!({"type":"generation"})).await;
        assert_eq!(status["application"], "current", "{status}");
        let rows = a.ok(json!({"type":"read","query":{"sql":"SELECT note FROM items","values":[],"max_rows":10}})).await;
        assert_eq!(
            rows["rows"],
            json!([[{"type":"text","value":"apples-migrated"}]])
        );
        let graph = ok(&a, "inspect", json!({})).await;
        assert_eq!(graph["seals"][0]["action"], "close");
        assert_eq!(graph["openings"].as_array().map(Vec::len), Some(1));
        a.close().await;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_migration_reads_its_frozen_parent_after_changing_the_live_rows() -> Result<(), Failure> {
    let root = common::identity::small_directory()?;
    let a = Device::create(root.path(), None)?;
    main(&a).await;
    ok(&a, "genesis_minimum", json!({"minimum_in_sync":1})).await;
    a.add(1, "apples").await;
    a.close().await;
    let mut declared: Value = serde_json::from_str(&schema(2, true))?;
    declared["migrations"][0]["statements"] = json!([
        "UPDATE items SET title=title || '-live'",
        "UPDATE items SET note=(SELECT p.title FROM parent.items p WHERE p.key=items.key)"
    ]);
    let declared = declared.to_string();
    let a = Device::new(Database::open(open(root.path(), &declared))?)?;
    let rows = a.ok(json!({"type":"read","query":{"sql":"SELECT title,note FROM items","values":[],"max_rows":10}})).await;
    assert_eq!(
        rows["rows"],
        json!([[{"type":"text","value":"apples-live"},{"type":"text","value":"apples"}]])
    );
    a.close().await;
    Ok(())
}
