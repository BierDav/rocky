//! The multi-file layout: names, assignment, refusal and resumed installs.
use super::*;
use crate::layout::file_path;
use crate::scaling_option_tests::{NODE, PEER, installations, note, open_node, scope};
use inventory_app::InventoryApp;
use rocky_core::*;
use rocky_platform::fs::Scratch;

fn files(files: usize) -> RuntimeOptions {
    RuntimeOptions {
        files,
        ..RuntimeOptions::default()
    }
}
/// Whether `domain`'s tables are in `path`.
async fn holds(path: &std::path::Path, domain: u8) -> bool {
    let conn = crate::sqlite::Connection::open_read_only(path).unwrap();
    let name = format!("{}_rocky_replica", crate::runtime::namespace(scope(domain)));
    crate::tests::rows(
        &conn,
        &format!("SELECT COUNT(*) FROM sqlite_schema WHERE name='{name}'"),
    )
    .await
    .unwrap()
        == vec![vec![SqlValue::Integer(1)]]
}
fn refused(result: Result<Runtime<InventoryApp>, Error>, expected: &str) {
    match result {
        Err(Error::Incompatible(message)) => assert!(message.contains(expected), "{message}"),
        Err(error) => panic!("{error}"),
        Ok(_) => panic!("opened: expected refusal containing {expected:?}"),
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn sibling_names_extend_the_stem() {
    let path = std::path::Path::new("/data/laptop.db");
    assert_eq!(file_path(path, 0), path);
    assert_eq!(
        file_path(path, 2),
        std::path::Path::new("/data/laptop-2.db")
    );
    assert_eq!(
        file_path(std::path::Path::new("/data/laptop"), 1),
        std::path::Path::new("/data/laptop-1")
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn domains_are_assigned_to_files_deterministically() {
    let dir = Scratch::new().await;
    let path = dir.path().join("laptop.db");
    // Two domains with four requested files use two files and two writers.
    let runtime = open_node(&path, NODE, files(4)).await;
    for domain in [1, 2] {
        note(&runtime.domain(scope(domain)).unwrap(), 1, "note")
            .committed()
            .await
            .unwrap();
    }
    runtime.close().await.unwrap();
    assert!(!rocky_platform::fs::exists(file_path(&path, 2)));
    let first = file_path(&path, 0);
    let second = file_path(&path, 1);
    assert!(holds(&first, 1).await && !holds(&first, 2).await);
    assert!(holds(&second, 2).await && !holds(&second, 1).await);
    let runtime = open_node(&path, NODE, files(4)).await;
    for domain in [1, 2] {
        let snapshot = runtime
            .domain(scope(domain))
            .unwrap()
            .query()
            .await
            .unwrap();
        assert_eq!(snapshot.revision.get(), 1);
    }
    runtime.close().await.unwrap();
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn another_file_count_or_a_missing_extra_or_foreign_file_is_refused() {
    let dir = Scratch::new().await;
    let path = dir.path().join("laptop.db");
    let open = |count| Runtime::open_with_options(&path, installations(NODE), files(count));
    open_node(&path, NODE, files(2))
        .await
        .close()
        .await
        .unwrap();
    let before = rocky_platform::fs::read(&path).unwrap();
    refused(open(1).await, "another file count");
    refused(open(3).await, "another file count");
    // A missing sibling, without creating it.
    let sibling = file_path(&path, 1);
    let moved = dir.path().join("moved.db");
    rocky_platform::fs::rename(&sibling, &moved).unwrap();
    refused(open(2).await, "missing");
    assert!(!rocky_platform::fs::exists(&sibling));
    rocky_platform::fs::rename(&moved, &sibling).unwrap();
    // A file beyond the recorded layout.
    let extra = file_path(&path, 2);
    rocky_platform::fs::write(&extra, b"").unwrap();
    refused(open(2).await, "not part of");
    rocky_platform::fs::remove_file(&extra).unwrap();
    // A sibling installed by another runtime.
    let other = dir.path().join("other.db");
    open_node(&other, PEER, files(2))
        .await
        .close()
        .await
        .unwrap();
    rocky_platform::fs::rename(&sibling, &moved).unwrap();
    rocky_platform::fs::copy(file_path(&other, 1), &sibling).unwrap();
    refused(open(2).await, "another runtime");
    rocky_platform::fs::remove_file(&sibling).unwrap();
    rocky_platform::fs::rename(&moved, &sibling).unwrap();
    assert_eq!(rocky_platform::fs::read(&path).unwrap(), before);
    open(2).await.unwrap().close().await.unwrap();
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn the_file_count_is_bounded() {
    let dir = Scratch::new().await;
    for count in [0, crate::runtime::MAX_FILES + 1] {
        let result =
            Runtime::open_with_options(dir.path().join("x.db"), installations(NODE), files(count))
                .await;
        assert!(matches!(result, Err(Error::Quota { .. })));
    }
}
