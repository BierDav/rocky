use super::*;
use crate::support::snapshot as snap;
use rocky_sdk::{Affinity, Column, Schema, SchemaBuilder};
use std::path::Path;

fn noted(version: u32) -> SchemaBuilder {
    common::shopping(version).table(common::items().column(Column::new("note", Affinity::Text)))
}
fn schema(version: u32, migration: bool) -> Schema {
    let schema = noted(version);
    common::build(match migration {
        true => schema.migration(version, ["UPDATE items SET note=title || '-migrated'"]),
        false => schema,
    })
}
fn open(directory: &Path, schema: &Schema) -> rocky_sdk::Configuration {
    crate::common::configuration(
        directory,
        schema,
        None,
        Some(common::external_identity(directory)),
        None,
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn additive_publication_moves_main_and_an_old_app_opens_behind() -> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let a = Device::create(root.path(), None).await?;
    a.add(1, "apples").await;
    let before = snap::ok(a.database.tip_status()).await["tip"].clone();
    a.close().await;
    let declared = schema(2, false);
    let a = Device::new(open(root.path(), &declared).open().await?)?;
    let status = snap::ok(async {
        a.database
            .status()
            .await
            .map(|status| status.data().generation)
    })
    .await;
    assert_eq!(status["application"], "current", "{status}");
    assert_eq!(status["current"]["schema"], 2);
    assert_ne!(snap::ok(a.database.tip_status()).await["tip"], before);
    assert_eq!(a.titles().await, ["apples"]);
    a.close().await;
    let a = Device::open(root.path(), None).await?;
    let status = snap::ok(async {
        a.database
            .status()
            .await
            .map(|status| status.data().generation)
    })
    .await;
    assert_eq!(status["application"], "behind", "{status}");
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn migration_closes_opens_a_new_line_and_commits_its_program_once() -> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let a = Device::create(root.path(), None).await?;
    a.add(1, "apples").await;
    a.close().await;
    let declared = schema(2, true);
    for _ in 0..2 {
        let a = Device::new(open(root.path(), &declared).open().await?)?;
        let status = snap::ok(async {
            a.database
                .status()
                .await
                .map(|status| status.data().generation)
        })
        .await;
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
        let graph = snap::ok(a.database.inspect_tips()).await;
        assert_eq!(graph["seals"][0]["action"], "close");
        assert_eq!(graph["openings"].as_array().map(Vec::len), Some(1));
        a.close().await;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_migration_reads_its_frozen_parent_after_changing_the_live_rows() -> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let a = Device::create(root.path(), None).await?;
    a.add(1, "apples").await;
    a.close().await;
    let parent_read_the_author_evaluates_once_for_replicas =
        "UPDATE items SET note=(SELECT max(p.title) FROM parent.items p)";
    let declared = common::build(noted(2).migration(
        2,
        [
            "UPDATE items SET title=title || '-live'",
            parent_read_the_author_evaluates_once_for_replicas,
        ],
    ));
    let a = Device::new(open(root.path(), &declared).open().await?)?;
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
