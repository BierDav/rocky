use super::*;
use crate::support::snapshot as snap;
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
        schema: crate::support::snapshot::schema(schema),
        identity: Some(common::external_identity(directory)),
        secrets: None,
        network: None,
        coordination: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn additive_publication_moves_main_and_an_old_app_opens_behind() -> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let a = Device::create(root.path(), None)?;
    a.add(1, "apples").await;
    let before = snap::ok(a.session.tip_status()).await["tip"].clone();
    a.close().await;
    let declared = schema(2, false);
    let a = Device::new(Database::open(open(root.path(), &declared))?)?
        .with_identity(common::external_identity(root.path()));
    let status = snap::ok(a.session.generation()).await;
    assert_eq!(status["application"], "current", "{status}");
    assert_eq!(status["current"]["schema"], 2);
    assert_ne!(snap::ok(a.session.tip_status()).await["tip"], before);
    assert_eq!(a.titles().await, ["apples"]);
    a.close().await;
    let a = Device::open(root.path(), None)?;
    let status = snap::ok(a.session.generation()).await;
    assert_eq!(status["application"], "behind", "{status}");
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn migration_closes_opens_a_new_line_and_commits_its_program_once() -> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let a = Device::create(root.path(), None)?;
    a.add(1, "apples").await;
    a.close().await;
    let declared = schema(2, true);
    for _ in 0..2 {
        let a = Device::new(Database::open(open(root.path(), &declared))?)?
            .with_identity(common::external_identity(root.path()));
        let status = snap::ok(a.session.generation()).await;
        assert_eq!(status["application"], "current", "{status}");
        let rows = snap::ok(snap::read(
            &a.database,
            "SELECT note FROM items",
            Vec::new(),
        ))
        .await;
        assert_eq!(
            rows["rows"],
            json!([[{"type":"text","value":"apples-migrated"}]])
        );
        let graph = snap::ok(a.session.inspect_tips()).await;
        assert_eq!(graph["seals"][0]["action"], "close");
        assert_eq!(graph["openings"].as_array().map(Vec::len), Some(1));
        a.close().await;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_migration_reads_its_frozen_parent_after_changing_the_live_rows() -> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let a = Device::create(root.path(), None)?;
    a.add(1, "apples").await;
    a.close().await;
    let mut declared: Value = serde_json::from_str(&schema(2, true))?;
    let parent_read_the_author_evaluates_once_for_replicas =
        "UPDATE items SET note=(SELECT max(p.title) FROM parent.items p)";
    declared["migrations"][0]["statements"] = json!([
        "UPDATE items SET title=title || '-live'",
        parent_read_the_author_evaluates_once_for_replicas
    ]);
    let declared = declared.to_string();
    let a = Device::new(Database::open(open(root.path(), &declared))?)?
        .with_identity(common::external_identity(root.path()));
    let rows = snap::ok(snap::read(
        &a.database,
        "SELECT title,note FROM items",
        Vec::new(),
    ))
    .await;
    assert_eq!(
        rows["rows"],
        json!([[{"type":"text","value":"apples-live"},{"type":"text","value":"apples"}]])
    );
    a.close().await;
    Ok(())
}
