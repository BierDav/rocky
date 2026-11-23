use super::*;

fn insert(f: &Fixture, row: u8) -> Vec<RowChange> {
    vec![change(
        f.public,
        row,
        None,
        Some(f.app().name(&format!("row-{row}"))),
    )]
}
async fn open(
    f: &Fixture,
    path: &std::path::Path,
    node: &PrivateKey,
) -> EncryptedReplica<Employees> {
    EncryptedReplica::open(path, f.app(), f.config(node, f.full(), &[f.public]))
        .await
        .unwrap()
}
async fn author(
    replica: &EncryptedReplica<Employees>,
    f: &Fixture,
    rows: std::ops::RangeInclusive<u8>,
) {
    for row in rows {
        replica
            .author_buffered([row; 32], insert(f, row), vec![])
            .await
            .unwrap();
    }
    replica.flush().await.unwrap();
}
async fn replayed_at_open(replica: &EncryptedReplica<Employees>) -> u64 {
    replica
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .audit_replayed
}
fn tamper(path: &std::path::Path, sql: &str) {
    let raw = rocky_storage::files::raw(path).unwrap();
    assert_eq!(raw.execute(sql, []).unwrap(), 1);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn an_open_replays_only_the_operations_after_its_audit_mark() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let key = PrivateKey::new();
    let path = dir.path().join("marked.db");
    let replica = open(&f, &path, &key).await;
    author(&replica, &f, 1..=6).await;
    drop(replica);
    let replica = open(&f, &path, &key).await;
    assert_eq!(
        replayed_at_open(&replica).await,
        6,
        "the first open after writing replays the unmarked history"
    );
    author(&replica, &f, 7..=9).await;
    drop(replica);
    let replica = open(&f, &path, &key).await;
    assert_eq!(
        replayed_at_open(&replica).await,
        3,
        "the audit mark advanced, so the next open verifies only the three new operations"
    );
    drop(replica);
    let replica = open(&f, &path, &key).await;
    assert_eq!(replayed_at_open(&replica).await, 0);
    replica.audit(f.public).await.unwrap();
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_corrupted_operation_after_the_audit_mark_refuses_the_open() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let key = PrivateKey::new();
    let path = dir.path().join("corrupted.db");
    let replica = open(&f, &path, &key).await;
    author(&replica, &f, 1..=4).await;
    drop(replica);
    let replica = open(&f, &path, &key).await;
    author(&replica, &f, 5..=5).await;
    drop(replica);
    tamper(
        &path,
        "UPDATE secure_operations SET body=substr(body,1,length(body)-1)||CASE WHEN substr(body,-1)=X'00' THEN X'01' ELSE X'00' END WHERE hash=(SELECT hash FROM secure_applied ORDER BY position DESC LIMIT 1)",
    );
    assert!(
        EncryptedReplica::open(&path, f.app(), f.config(&key, f.full(), &[f.public]))
            .await
            .is_err(),
        "an operation after the mark is verified at open"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_projection_row_changed_without_an_operation_refuses_the_open() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let key = PrivateKey::new();
    let path = dir.path().join("projection.db");
    let replica = open(&f, &path, &key).await;
    author(&replica, &f, 1..=4).await;
    drop(replica);
    drop(open(&f, &path, &key).await);
    tamper(
        &path,
        "WITH marked AS (SELECT scope FROM secure_rows GROUP BY scope ORDER BY count(*) DESC LIMIT 1) UPDATE secure_rows SET fields=(SELECT fields FROM secure_rows WHERE scope=(SELECT scope FROM marked) ORDER BY row DESC LIMIT 1) WHERE scope=(SELECT scope FROM marked) AND row=(SELECT row FROM secure_rows WHERE scope=(SELECT scope FROM marked) ORDER BY row LIMIT 1)",
    );
    assert!(
        EncryptedReplica::open(&path, f.app(), f.config(&key, f.full(), &[f.public]))
            .await
            .is_err(),
        "rows the mark covers are still checked against signed history"
    );
}
