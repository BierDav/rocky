use super::*;

#[tokio::test]
async fn omitted_global_claim_head_and_off_custody_ledgers_are_rechecked() {
    let f = AuditFixture::new(false).await;
    let items = f.lineage.scope("items");
    let tags = f.lineage.scope("tags");
    for (table, attack) in [
        (
            "secure_admission_claims",
            "UPDATE secure_admission_claims SET ordinal=31 WHERE entry=(SELECT max(entry) FROM secure_admission_entries)",
        ),
        (
            "secure_admission_entries",
            "UPDATE secure_admission_entries SET signature=zeroblob(64) WHERE entry=(SELECT max(entry) FROM secure_admission_entries)",
        ),
        (
            "secure_admission_context",
            "UPDATE secure_admission_context SET head_signature=zeroblob(64)",
        ),
        (
            "secure_identity_ledger",
            "DELETE FROM secure_identity_ledger WHERE scope=?",
        ),
    ] {
        let before = snapshot(&f.replica).await;
        let inner = f.replica.inner.lock().await;
        let saved = rows(&inner.conn, &format!("SELECT * FROM {table}"))
            .await
            .unwrap();
        let saved_tables = inner.tables.clone();
        let roles = rows(&inner.conn, "SELECT * FROM secure_roles")
            .await
            .unwrap();
        let subscriptions = rows(&inner.conn, "SELECT * FROM secure_subscriptions")
            .await
            .unwrap();
        if table == "secure_identity_ledger" {
            inner.conn.execute("INSERT INTO secure_roles VALUES(1,?) ON CONFLICT(id) DO UPDATE SET config=excluded.config",[encode(&crate::RoleConfig { role:crate::ReplicaRole::Partial,..Default::default() }).unwrap()]).await.unwrap();
            inner.conn.execute("INSERT INTO secure_subscriptions VALUES(?,'left') ON CONFLICT(scope) DO UPDATE SET state=excluded.state",[encode(&tags).unwrap()]).await.unwrap();
            assert!(!inner.holds(tags).await.unwrap());
            inner
                .conn
                .execute(
                    "UPDATE secure_subscriptions SET state='left' WHERE scope=?",
                    [encode(&tags).unwrap()],
                )
                .await
                .unwrap();
            inner
                .conn
                .execute(attack, [encode(&tags).unwrap()])
                .await
                .unwrap();
        } else {
            inner.conn.execute(attack, ()).await.unwrap();
        }
        drop(inner);
        assert!(
            matches!(
                f.replica.audit_scopes(&[items]).await,
                Err(Error::Corrupt(_))
            ),
            "{attack}"
        );
        let mut inner = f.replica.inner.lock().await;
        inner.tables = saved_tables;
        if table == "secure_identity_ledger" {
            inner
                .conn
                .execute(
                    "UPDATE secure_subscriptions SET state='held' WHERE scope=?",
                    [encode(&tags).unwrap()],
                )
                .await
                .unwrap();
        }
        inner
            .conn
            .execute(format!("DELETE FROM {table}"), ())
            .await
            .unwrap();
        for row in saved {
            inner
                .conn
                .execute(
                    format!(
                        "INSERT INTO {table} VALUES({})",
                        vec!["?"; row.len()].join(",")
                    ),
                    bindings(row),
                )
                .await
                .unwrap();
        }
        for (table, saved) in [
            ("secure_roles", roles),
            ("secure_subscriptions", subscriptions),
        ] {
            inner
                .conn
                .execute(format!("DELETE FROM {table}"), ())
                .await
                .unwrap();
            for row in saved {
                inner
                    .conn
                    .execute(
                        format!(
                            "INSERT INTO {table} VALUES({})",
                            vec!["?"; row.len()].join(",")
                        ),
                        bindings(row),
                    )
                    .await
                    .unwrap();
            }
        }
        drop(inner);
        assert_eq!(snapshot(&f.replica).await, before);
        f.replica.audit_scopes(&[items]).await.unwrap();
    }
}

#[tokio::test]
async fn omitted_queued_empty_and_keyless_ledgers_remain_valid() {
    let f = AuditFixture::new(false).await;
    let items = f.lineage.scope("items");
    let tags = f.lineage.scope("tags");
    let mut inner = f.replica.inner.lock().await;
    inner.config.keys.retain(|key| key.scope != tags);
    inner
        .conn
        .execute(
            "INSERT INTO secure_rebuild_scopes VALUES(?)",
            [encode(&tags).unwrap()],
        )
        .await
        .unwrap();
    inner
        .conn
        .execute(
            "DELETE FROM secure_identity_ledger WHERE scope=?",
            [encode(&tags).unwrap()],
        )
        .await
        .unwrap();
    drop(inner);
    let before = snapshot(&f.replica).await;
    f.replica.audit_scopes(&[items, tags]).await.unwrap();
    assert_eq!(snapshot(&f.replica).await, before);
}
