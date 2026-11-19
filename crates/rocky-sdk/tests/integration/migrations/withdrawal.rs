use super::{LISTED, UNLISTED_FAILING, generation, open, reopen};
use crate::common::{self, Device, Failure};
use crate::support::snapshot as snap;
use rocky_sdk::Database;
use serde_json::Value;
use std::path::Path;

fn orphan_every_item_list(directory: &Path) -> Result<(), Failure> {
    let file = rusqlite::Connection::open(directory.join("rocky.sqlite"))?;
    file.execute_batch("PRAGMA foreign_keys=OFF; UPDATE items SET list = x'00';")?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_migration_whose_withdrawal_is_refused_opens_on_its_schema_until_withdrawn()
-> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let a = Device::new(Database::create(open(root.path(), LISTED, None), None)?)?
        .with_identity(common::external_identity(root.path()));
    a.follow_main_migrating().await;
    a.add(1, "apples").await;
    a.close().await;
    orphan_every_item_list(root.path())?;

    let failed = Database::open(open(root.path(), UNLISTED_FAILING, None))
        .err()
        .ok_or("a failing migration opened")?;
    assert!(
        failed
            .to_string()
            .contains("migration to version 2 failed at statement 1"),
        "{failed}"
    );
    let a = reopen(open(root.path(), UNLISTED_FAILING, None))
        .map_err(|refused| format!("a refused withdrawal leaves its schema openable: {refused}"))?;
    let status = generation(&a).await;
    assert_eq!(status["current"]["schema"], 2, "{status}");
    assert_eq!(
        status["migration"]["state"], "withdrawal_refused",
        "{status}"
    );
    assert!(
        status["migration"]["reason"].as_str().is_some_and(
            |reason| reason.contains("a row of items references a missing row of lists")
        ),
        "the app sees which rows refuse the withdrawal: {status}"
    );
    assert_eq!(a.titles().await, ["apples"]);
    a.close().await;

    let a = reopen(open(root.path(), UNLISTED_FAILING, None))?;
    let still_refused = snap::value(a.session.withdraw_failed_migration())
        .await
        .err()
        .ok_or("a withdrawal over the same rows succeeded")?;
    assert!(
        still_refused
            .to_string()
            .contains("a row of items references a missing row of lists"),
        "{still_refused}"
    );
    snap::ok(a.session.execute("BEGIN", Vec::new())).await;
    snap::ok(
        a.session
            .execute("UPDATE items SET list = NULL", Vec::new()),
    )
    .await;
    snap::ok(a.session.commit([3; 32])).await;
    let status = snap::ok(a.session.withdraw_failed_migration()).await;
    assert_eq!(status["current"]["schema"], 1, "{status}");
    assert_eq!(status["migration"], Value::Null, "{status}");
    assert_eq!(a.titles().await, ["apples"]);
    a.close().await;

    let a = reopen(open(root.path(), LISTED, None))?;
    let status = generation(&a).await;
    assert_eq!(status["current"]["schema"], 1, "{status}");
    assert_eq!(status["application"], "current");
    assert_eq!(a.titles().await, ["apples"]);
    a.close().await;
    Ok(())
}
