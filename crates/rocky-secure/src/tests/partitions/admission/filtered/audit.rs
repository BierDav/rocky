use super::*;

#[tokio::test]
async fn an_omitted_removed_partition_ledger_is_still_verified() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::build_with(true, PrivateKey::from_bytes(&[230; 32]));
    let writer = keyed(&f, &root.path().join("writer"), [231; 32], 1).await;
    let reader = keyed(&f, &root.path().join("reader"), [232; 32], 2).await;
    let frame = writer
        .author(
            [1; 32],
            vec![f.titled("A", 1, 1, "retained").change],
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(reader.receive(frame).await.unwrap(), ReceiveStatus::Applied);
    let (absent, statements) = removed_path(
        &f,
        writer.followed_tip().await,
        vec![AuthorMark {
            author: 1,
            sequence: 0,
        }],
    );
    reader.receive_tip_statements(&statements).await.unwrap();
    switch(&f, &reader, "dev", &absent).await;
    assert!(
        !reader
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .tables
            .contains(&f.scope("A").table)
    );
    assert!(!reader.key_epochs(f.scope("A")).await.is_empty());
    reader.audit_scopes(&[f.scope("B")]).await.unwrap();
    let before = crate::tests::compaction::audit::snapshot(&reader).await;
    let inner = reader.inner.lock(crate::replica::Class::Background).await;
    let saved = rows(&inner.conn, "SELECT * FROM secure_identity_ledger")
        .await
        .unwrap();
    // Inject an inconsistent L from the retained real birth mark, within O/P.
    assert_eq!(inner.conn.execute("INSERT OR REPLACE INTO secure_identity_ledger SELECT scope,author,signer,sequence FROM secure_identity_marks WHERE scope=?", [encode(&f.scope("A")).unwrap()]).await.unwrap(), 1);
    drop(inner);
    let error = reader.audit_scopes(&[f.scope("B")]).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("identity ledger or summary differs"),
        "{error}"
    );
    let inner = reader.inner.lock(crate::replica::Class::Background).await;
    inner
        .conn
        .execute("DELETE FROM secure_identity_ledger", ())
        .await
        .unwrap();
    for record in saved {
        inner
            .conn
            .execute(
                "INSERT INTO secure_identity_ledger VALUES(?,?,?,?)",
                crate::storage::bindings(record),
            )
            .await
            .unwrap();
    }
    drop(inner);
    assert_eq!(
        crate::tests::compaction::audit::snapshot(&reader).await,
        before
    );
    reader.audit_scopes(&[f.scope("B")]).await.unwrap();
}

type Journal = Vec<(&'static str, Vec<Vec<SqlValue>>)>;

pub(super) async fn journal(replica: &EncryptedReplica<TypedRows>) -> Journal {
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    let mut saved = Vec::new();
    for table in [
        "secure_admission_entries",
        "secure_admission_claims",
        "secure_admission_carries",
        "secure_admission_context",
    ] {
        saved.push((
            table,
            rows(&inner.conn, &format!("SELECT * FROM {table}"))
                .await
                .unwrap(),
        ));
    }
    saved
}

async fn replace(replica: &EncryptedReplica<TypedRows>, saved: Journal) {
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    for (table, records) in saved {
        inner
            .conn
            .execute(format!("DELETE FROM {table}"), ())
            .await
            .unwrap();
        for record in records {
            inner
                .conn
                .execute(
                    format!(
                        "INSERT INTO {table} VALUES({})",
                        vec!["?"; record.len()].join(",")
                    ),
                    crate::storage::bindings(record),
                )
                .await
                .unwrap();
        }
    }
    inner.invalidate_context();
    inner.finish_admission_revision().await.unwrap();
}

pub(super) async fn refuses_missing_continuation(
    replica: &EncryptedReplica<TypedRows>,
    scope: ResourceScope,
    prefix: Journal,
    message: &str,
) {
    let complete = journal(replica).await;
    // Privileged fixture removal of the real continuation, keeping its signed prefix.
    replace(replica, prefix).await;
    let before = crate::tests::compaction::audit::snapshot(replica).await;
    let error = replica.audit_scopes(&[scope]).await.unwrap_err();
    assert!(matches!(error, Error::Corrupt(_)), "{error}");
    assert!(error.to_string().contains(message), "{error}");
    assert_eq!(replica.audit_scratch_flags().await, (false, false));
    assert!(
        rows(
            &replica
                .inner
                .lock(crate::replica::Class::Background)
                .await
                .conn,
            "SELECT 1 FROM secure_checkpoint_scratch"
        )
        .await
        .unwrap()
        .is_empty()
    );
    assert_eq!(
        crate::tests::compaction::audit::snapshot(replica).await,
        before
    );
    replace(replica, complete).await;
    let single = replica.audit(scope).await.unwrap();
    assert_eq!(
        replica.audit_scopes(&[scope]).await.unwrap(),
        vec![(scope, single)]
    );
}
