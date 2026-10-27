//! Local admission evidence survives row reclamation and fresh-node adoption.
mod bindings;
use super::*;
use crate::storage::{bindings, rows};

async fn records(replica: &Replica, sql: &str) -> Vec<Vec<SqlValue>> {
    rows(
        &replica
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .conn,
        sql,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn shared_frame_verification_still_checks_each_current_sql_claim() {
    let lineage = Lineage::pinned(237);
    let dir = TempDir::new().unwrap();
    let key = PrivateKey::from_bytes(&[238; 32]);
    let replica = open(&lineage, &dir, "claims", &key).await;
    follow_main(&lineage, &replica).await;
    let sql = session(&replica).await;
    sql.run("BEGIN", vec![]).await.unwrap();
    for name in ["one", "two"] {
        identity_insert(
            &sql,
            "INSERT INTO items(key,title) VALUES(?,?)",
            vec![text(name), text(name)],
        )
        .await
        .unwrap();
    }
    sql.run("COMMIT", vec![]).await.unwrap();
    let original = records(
        &replica,
        "SELECT * FROM secure_admission_claims ORDER BY ordinal",
    )
    .await;
    assert_eq!(original.len(), 2);
    assert_eq!(
        records(&replica, "SELECT count(*) FROM secure_admission_entries").await,
        vec![vec![SqlValue::Integer(1)]]
    );
    let identities = original
        .iter()
        .map(|row| match row[1] {
            SqlValue::Integer(identity) => rocky_core::RowIdentity::from_integer(identity).unwrap(),
            _ => panic!("claim identity"),
        })
        .collect::<Vec<_>>();
    let scope = lineage.scope("items");
    let error = replica
        .change_claim_after_frame_verification_for_test(scope, [identities[0], identities[1]])
        .await
        .unwrap_err();
    assert!(
        matches!(error, Error::Corrupt(ref reason) if reason == "admission evidence claim differs from its entry"),
        "{error}"
    );
    assert_eq!(
        records(
            &replica,
            "SELECT * FROM secure_admission_claims ORDER BY ordinal"
        )
        .await,
        original
    );
    replica.rebuild_projection(scope).await.unwrap();
    replica.audit(scope).await.unwrap();
}

#[tokio::test]
async fn journal_tampering_never_becomes_an_empty_admission_history() {
    let lineage = Lineage::pinned(101);
    let dir = TempDir::new().unwrap();
    let key = PrivateKey::from_bytes(&[102; 32]);
    let replica = open(&lineage, &dir, "a", &key).await;
    follow_main(&lineage, &replica).await;
    insert(&replica, "one", "first").await;
    insert(&replica, "two", "second").await;
    let entries = records(
        &replica,
        "SELECT * FROM secure_admission_entries ORDER BY entry",
    )
    .await;
    let claims = records(&replica, "SELECT * FROM secure_admission_claims").await;
    let context = records(&replica, "SELECT * FROM secure_admission_context").await;
    let own = records(&replica, "SELECT * FROM secure_identity_own").await;
    assert_eq!(entries.len(), 2);
    assert_eq!(claims.len(), 2);
    for attack in [
        "UPDATE secure_admission_entries SET bytes=CAST(bytes||X'00' AS BLOB) WHERE entry=1",
        "UPDATE secure_admission_entries SET signature=zeroblob(64) WHERE entry=1",
        "UPDATE secure_admission_entries SET chain=zeroblob(32) WHERE entry=1",
        "UPDATE secure_admission_entries SET scope=X'00' WHERE entry=1",
        "UPDATE secure_admission_claims SET ordinal=31 WHERE entry=1",
        "DELETE FROM secure_admission_claims WHERE entry=1",
        "UPDATE secure_admission_context SET head_signature=zeroblob(64)",
        "DELETE FROM secure_admission_entries WHERE entry=2",
        "DELETE FROM secure_admission_entries WHERE entry=1",
        "UPDATE secure_identity_own SET sequence=4294967295",
        "DELETE FROM secure_identity_own",
    ] {
        replica
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .conn
            .execute(attack, ())
            .await
            .unwrap();
        let error = replica.audit(lineage.scope("items")).await.unwrap_err();
        assert!(matches!(error, Error::Corrupt(_)), "{attack}: {error}");
        let inner = replica.inner.lock(crate::replica::Class::Background).await;
        for table in [
            "secure_admission_entries",
            "secure_admission_claims",
            "secure_admission_context",
            "secure_identity_own",
        ] {
            inner
                .conn
                .execute(format!("DELETE FROM {table}"), ())
                .await
                .unwrap();
        }
        for (table, values) in [
            ("secure_admission_entries", &entries),
            ("secure_admission_claims", &claims),
            ("secure_admission_context", &context),
            ("secure_identity_own", &own),
        ] {
            for row in values {
                inner
                    .conn
                    .execute(
                        format!(
                            "INSERT INTO {table} VALUES({})",
                            vec!["?"; row.len()].join(",")
                        ),
                        bindings(row.clone()),
                    )
                    .await
                    .unwrap();
            }
        }
        drop(inner);
        replica.audit(lineage.scope("items")).await.unwrap();
    }
    drop(replica);
    let reopened = lineage
        .reopen(&dir.path().join("a.db"), &key, 3, 3)
        .await
        .unwrap();
    reopened.audit(lineage.scope("items")).await.unwrap();
    assert_eq!(
        records(
            &reopened,
            "SELECT * FROM secure_admission_entries ORDER BY entry"
        )
        .await,
        entries
    );
}

#[tokio::test]
async fn missing_mandatory_evidence_is_refused_before_schema_creation() {
    let lineage = Lineage::pinned(219);
    let dir = TempDir::new().unwrap();
    let key = PrivateKey::from_bytes(&[220; 32]);
    let replica = open(&lineage, &dir, "source", &key).await;
    follow_main(&lineage, &replica).await;
    insert(&replica, "one", "admitted").await;
    for (index, attack) in [
        "DROP TABLE secure_identity_own",
        "DROP TABLE secure_admission_carries",
        "DROP TABLE secure_position_ranges",
        "DROP TABLE secure_log_ranges",
        "DROP TABLE secure_existence_counts",
        "DROP TABLE secure_position_sources",
        "DROP TRIGGER secure_positions_status",
        "DELETE FROM secure_admission_context",
    ]
    .into_iter()
    .enumerate()
    {
        let path = dir.path().join(format!("damaged{index}.db"));
        replica.backup_into(&path).await.unwrap();
        let (owner, conn) = crate::database::DatabaseOwner::open(&path).await.unwrap();
        conn.execute(attack, ()).await.unwrap();
        drop(conn);
        drop(owner);
        let result = lineage.reopen(&path, &key, 3, 3).await;
        assert!(
            matches!(result, Err(Error::Corrupt(_))),
            "{attack}: {:?}",
            result.err()
        );
        // Failed open never backfills a missing mandatory table.
        let (owner, conn) = crate::database::DatabaseOwner::open(&path).await.unwrap();
        if let Some(name) = attack
            .strip_prefix("DROP TABLE ")
            .or_else(|| attack.strip_prefix("DROP TRIGGER "))
        {
            assert!(
                crate::storage::collect_rows(
                    conn.query("SELECT 1 FROM sqlite_schema WHERE name=?", [name])
                        .await
                        .unwrap()
                )
                .await
                .unwrap()
                .is_empty()
            );
        }
        drop(conn);
        drop(owner);
    }
}

#[tokio::test]
async fn two_fresh_node_restores_keep_finite_cuts_and_new_admissions() {
    let lineage = Lineage::pinned(105);
    let dir = TempDir::new().unwrap();
    let keys = [106, 107, 108].map(|seed| PrivateKey::from_bytes(&[seed; 32]));
    let source = open(&lineage, &dir, "source", &keys[0]).await;
    follow_main(&lineage, &source).await;
    insert(&source, "one", "original").await;
    let original = records(
        &source,
        "SELECT * FROM secure_admission_entries ORDER BY entry",
    )
    .await;
    let first_path = dir.path().join("first.db");
    source.backup_into(&first_path).await.unwrap();
    let first = EncryptedReplica::open_restored(
        &first_path,
        lineage.app(3),
        lineage.config(3, &keys[1]),
        keys[0].public_key(),
    )
    .await
    .unwrap();
    assert_eq!(
        first.predecessors().await.unwrap(),
        vec![keys[0].public_key()]
    );
    assert_eq!(
        records(
            &first,
            "SELECT * FROM secure_admission_entries ORDER BY entry"
        )
        .await,
        original
    );
    first
        .register_line_node(crate::LineNode {
            key: keys[1].clone(),
            credentials: lineage.credentials(3, &keys[1]),
        })
        .await
        .unwrap();
    insert(&first, "two", "first restore").await;
    first.audit(lineage.scope("items")).await.unwrap();
    let second_path = dir.path().join("second.db");
    first.backup_into(&second_path).await.unwrap();
    let second = EncryptedReplica::open_restored(
        &second_path,
        lineage.app(3),
        lineage.config(3, &keys[2]),
        keys[1].public_key(),
    )
    .await
    .unwrap();
    assert_eq!(
        second.predecessors().await.unwrap(),
        vec![keys[0].public_key(), keys[1].public_key()]
    );
    second
        .register_line_node(crate::LineNode {
            key: keys[2].clone(),
            credentials: lineage.credentials(3, &keys[2]),
        })
        .await
        .unwrap();
    insert(&second, "three", "second restore").await;
    second.audit(lineage.scope("items")).await.unwrap();
    let expected = records(
        &second,
        "SELECT * FROM secure_admission_entries ORDER BY entry",
    )
    .await;
    assert_eq!(expected.len(), 3);
    assert_eq!(expected[0], original[0]);
    drop(second);
    let second = lineage.reopen(&second_path, &keys[2], 3, 3).await.unwrap();
    second
        .rebuild_projection(lineage.scope("items"))
        .await
        .unwrap();
    second.audit(lineage.scope("items")).await.unwrap();
    assert_eq!(
        records(
            &second,
            "SELECT * FROM secure_admission_entries ORDER BY entry"
        )
        .await,
        expected
    );
    assert_eq!(titles(&second).await.len(), 3);
}

#[tokio::test]
async fn a_restore_cut_never_adopts_later_predecessor_or_sibling_receipts() {
    let lineage = Lineage::pinned(239);
    let dir = TempDir::new().unwrap();
    let keys = [240, 241, 242].map(|seed| PrivateKey::from_bytes(&[seed; 32]));
    let source = open(&lineage, &dir, "source", &keys[0]).await;
    follow_main(&lineage, &source).await;
    insert(&source, "one", "before the cut").await;
    let mut restored = Vec::new();
    for (index, key) in keys[1..].iter().enumerate() {
        let path = dir.path().join(format!("restored{index}.db"));
        source.backup_into(&path).await.unwrap();
        let replica = EncryptedReplica::open_restored(
            &path,
            lineage.app(3),
            lineage.config(3, key),
            keys[0].public_key(),
        )
        .await
        .unwrap();
        replica
            .register_line_node(crate::LineNode {
                key: key.clone(),
                credentials: lineage.credentials(3, key),
            })
            .await
            .unwrap();
        insert(&replica, "two", "after adoption").await;
        replica.audit(lineage.scope("items")).await.unwrap();
        restored.push(replica);
    }
    insert(&source, "two", "after the backup").await;
    let target = &restored[0];
    let original = records(
        target,
        "SELECT bytes,signature FROM secure_admission_entries WHERE entry=2",
    )
    .await;
    let head = records(target, "SELECT * FROM secure_admission_context").await;
    let projection = titles(target).await;
    for outsider in [&source, &restored[1]] {
        // Each signature is valid in its own file at this index. The adopted
        // range ends at 1; entry 2 belongs exclusively to the restoring node.
        outsider.audit(lineage.scope("items")).await.unwrap();
        let foreign = records(
            outsider,
            "SELECT bytes,signature FROM secure_admission_entries WHERE entry=2",
        )
        .await;
        assert_eq!(foreign.len(), 1);
        assert_ne!(foreign, original);
        target
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .conn
            .execute(
                "UPDATE secure_admission_entries SET bytes=?,signature=? WHERE entry=2",
                bindings(foreign[0].clone()),
            )
            .await
            .unwrap();
        let error = target.audit(lineage.scope("items")).await.unwrap_err();
        assert!(
            error.to_string().contains("signature does not verify"),
            "{error}"
        );
        assert_eq!(titles(target).await, projection);
        assert_eq!(
            records(target, "SELECT * FROM secure_admission_context").await,
            head
        );
        target
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .conn
            .execute(
                "UPDATE secure_admission_entries SET bytes=?,signature=? WHERE entry=2",
                bindings(original[0].clone()),
            )
            .await
            .unwrap();
        target.audit(lineage.scope("items")).await.unwrap();
    }
    let target = restored.remove(0);
    drop(target);
    let target = lineage
        .reopen(&dir.path().join("restored0.db"), &keys[1], 3, 3)
        .await
        .unwrap();
    target.audit(lineage.scope("items")).await.unwrap();
    assert_eq!(titles(&target).await, projection);
}

#[tokio::test]
async fn a_revoked_fresh_restore_config_does_not_rebind_the_backup() {
    let lineage = Lineage::pinned(249);
    let dir = TempDir::new().unwrap();
    let keys = [250, 251, 252].map(|seed| PrivateKey::from_bytes(&[seed; 32]));
    let source = open(&lineage, &dir, "source", &keys[0]).await;
    insert(&source, "one", "before backup").await;
    let path = dir.path().join("restored.db");
    source.backup_into(&path).await.unwrap();
    let mut rejected = lineage.config(3, &keys[1]);
    rejected.revocations = crate::tests::revoking(
        lineage.publisher(),
        lineage.domain.app,
        &[keys[1].public_key()],
        &[],
    );
    let result =
        EncryptedReplica::open_restored(&path, lineage.app(3), rejected, keys[0].public_key())
            .await;
    let error = match result {
        Ok(_) => panic!("revoked fresh identity cannot adopt a backup"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("revoked"), "{error}");
    // A different qualified fresh node can still adopt the untouched identity.
    let restored = EncryptedReplica::open_restored(
        &path,
        lineage.app(3),
        lineage.config(3, &keys[2]),
        keys[0].public_key(),
    )
    .await
    .unwrap();
    assert_eq!(
        restored.predecessors().await.unwrap(),
        vec![keys[0].public_key()]
    );
    assert_eq!(
        titles(&restored).await,
        vec![vec![text("one"), text("before backup")]]
    );
    restored.audit(lineage.scope("items")).await.unwrap();
}

#[derive(Debug)]
pub(super) struct Accounting {
    pub entries: i64,
    pub grants: i64,
    pub chains: i64,
    pub bounded: Vec<i64>,
    pub logical_bytes: usize,
}

pub(super) async fn accounting(
    replica: &Replica,
    path: &std::path::Path,
    cycle: usize,
) -> Accounting {
    let tables = [
        "secure_admission_entries",
        "secure_admission_claims",
        "secure_admission_carries",
        "secure_admission_context",
        "secure_identity_marks",
        "secure_identity_ledger",
        "secure_identity_summary",
        "secure_identity_own",
        "secure_identity_tables",
        "secure_identity_partitions",
    ];
    let mut logical_bytes = 0;
    // Logical integer fields count eight bytes; blobs count their encoded length.
    for table in tables {
        let query = if table == "secure_admission_entries" {
            "SELECT bytes,signature,chain FROM secure_admission_entries".to_string()
        } else {
            format!("SELECT * FROM {table}")
        };
        for row in records(replica, &query).await {
            for value in row {
                logical_bytes += match value {
                    SqlValue::Integer(_) | SqlValue::Real(_) => 8,
                    SqlValue::Blob(bytes) => bytes.len(),
                    SqlValue::Text(text) => text.len(),
                    SqlValue::Null => 0,
                };
            }
        }
    }
    let mut bounded = Vec::new();
    for table in [
        "secure_identity_marks",
        "secure_identity_summary",
        "secure_identity_own",
        "secure_identity_ledger",
        "secure_identity_tables",
        "secure_identity_partitions",
    ] {
        bounded.push(scalar(replica, &format!("SELECT count(*) FROM {table}")).await);
    }
    let mut inner = replica.inner.lock(crate::replica::Class::Background).await;
    inner.flush().unwrap();
    let modules = rows(
        &inner.conn,
        "SELECT name FROM pragma_module_list WHERE name='dbstat'",
    )
    .await
    .unwrap();
    let allocated = if modules.is_empty() {
        rows(&inner.conn, "PRAGMA page_count").await.unwrap()
    } else {
        rows(&inner.conn, "SELECT name,sum(pgsize) FROM dbstat WHERE name IN (SELECT name FROM sqlite_schema WHERE tbl_name LIKE 'secure_admission_%' OR tbl_name LIKE 'secure_identity_%') GROUP BY name ORDER BY name").await.unwrap()
    };
    let operation_pages = if modules.is_empty() {
        Vec::new()
    } else {
        rows(&inner.conn, "SELECT name,sum(pgsize) FROM dbstat WHERE name IN (SELECT name FROM sqlite_schema WHERE tbl_name='secure_operations') GROUP BY name ORDER BY name").await.unwrap()
    };
    let pages = rows(&inner.conn, "PRAGMA page_count").await.unwrap();
    let free = rows(&inner.conn, "PRAGMA freelist_count").await.unwrap();
    let mut wal_path = path.as_os_str().to_owned();
    wal_path.push("-wal");
    let wal_path = std::path::PathBuf::from(wal_path);
    let wal_bytes = || match std::fs::metadata(&wal_path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => panic!("WAL accounting: {error}"),
    };
    let wal_before = wal_bytes();
    rows(&inner.conn, "PRAGMA wal_checkpoint(TRUNCATE)")
        .await
        .unwrap();
    drop(inner);
    let main = std::fs::metadata(path).unwrap().len();
    let wal = wal_bytes();
    let graph = scalar(
        replica,
        "SELECT COALESCE(sum(length(statement)),0) FROM secure_tip_statements",
    )
    .await;
    println!(
        "admission accounting cycles={cycle}: logical={logical_bytes}B; allocated={allocated:?}; operation objects={operation_pages:?}; pages/free={pages:?}/{free:?}; main={main}B; WAL before/after truncate={wal_before}/{wal}B; graph={graph}B (separate)"
    );
    Accounting {
        entries: scalar(
            replica,
            "SELECT count(*) FROM secure_admission_entries WHERE kind=0",
        )
        .await,
        grants: scalar(
            replica,
            "SELECT count(*) FROM secure_admission_claims WHERE kind=0",
        )
        .await,
        chains: scalar(replica, "SELECT count(chain) FROM secure_admission_entries").await,
        bounded,
        logical_bytes,
    }
}

#[tokio::test]
async fn a_failed_admission_rolls_back_its_receipt_with_every_record() {
    let lineage = Lineage::pinned(107);
    let dir = TempDir::new().unwrap();
    let keys = [108, 109].map(|seed| PrivateKey::from_bytes(&[seed; 32]));
    let a = open(&lineage, &dir, "a", &keys[0]).await;
    let b = open(&lineage, &dir, "b", &keys[1]).await;
    follow_main(&lineage, &a).await;
    b.receive_tip_statements(&crate::tests::all_tip_statements(&a).await)
        .await
        .unwrap();
    b.follow_bookmark(lineage.domain, "main").await.unwrap();
    let sql = session(&a).await;
    sql.run("BEGIN", vec![]).await.unwrap();
    for (key, title) in [("one", "first"), ("two", "second")] {
        identity_insert(
            &sql,
            "INSERT INTO items(key,title) VALUES(?,?)",
            vec![text(key), text(title)],
        )
        .await
        .unwrap();
    }
    sql.run("COMMIT", vec![]).await.unwrap();
    let frames = crate::tests::stored_frames(&a, "SELECT header,body FROM secure_operations").await;
    assert_eq!(frames.len(), 1);
    let state = "SELECT 'e',entry,bytes,chain FROM secure_admission_entries UNION ALL SELECT 'c',identity,entry,provenance FROM secure_admission_claims UNION ALL SELECT 'x',context,head,signature FROM secure_admission_context UNION ALL SELECT 'p',scope,author,sequence FROM secure_identity_marks UNION ALL SELECT 'l',scope,author,sequence FROM secure_identity_ledger UNION ALL SELECT 'O',scope,author,sequence FROM secure_identity_own UNION ALL SELECT 'r',revision,0,0 FROM secure_metadata UNION ALL SELECT 'o',hash,status,0 FROM secure_operations";
    b.stage_bundle_before_apply_for_test(&frames).await.unwrap();
    let before = records(&b, state).await;
    let full_before = crate::tests::bundles::payload::state(&b).await;
    b.inject_group_fault(Some(
        crate::replica::group::GroupFault::FailApplicationBeforeCommit,
    ))
    .await;
    crate::BINDING_TRACE.with(|trace| *trace.borrow_mut() = Some(Vec::new()));
    assert!(b.receive(frames[0].clone()).await.is_err());
    assert_eq!(
        crate::BINDING_TRACE.with(|trace| trace.borrow_mut().take().unwrap()),
        vec![(2, 1, 1)]
    );
    assert_eq!(records(&b, state).await, before);
    assert_eq!(crate::tests::bundles::payload::state(&b).await, full_before);
    b.inject_group_fault(None).await;
    crate::BINDING_TRACE.with(|trace| *trace.borrow_mut() = Some(Vec::new()));
    b.receive(frames[0].clone()).await.unwrap();
    assert_eq!(
        crate::BINDING_TRACE.with(|trace| trace.borrow_mut().take().unwrap()),
        vec![(2, 1, 1)]
    );
    assert_eq!(
        scalar(&b, "SELECT count(*) FROM secure_admission_entries").await,
        1
    );
    assert_eq!(
        scalar(
            &b,
            "SELECT count(*) FROM secure_admission_claims WHERE kind=0"
        )
        .await,
        2
    );
    assert_eq!(
        records(
            &b,
            "SELECT ordinal FROM secure_admission_claims ORDER BY ordinal"
        )
        .await,
        vec![vec![SqlValue::Integer(0)], vec![SqlValue::Integer(1)]]
    );
    assert_eq!(
        titles(&b).await,
        vec![
            vec![text("one"), text("first")],
            vec![text("two"), text("second")]
        ]
    );
    b.receive(frames[0].clone()).await.unwrap();
    assert_eq!(
        scalar(&b, "SELECT count(*) FROM secure_admission_entries").await,
        1
    );
    b.audit(lineage.scope("items")).await.unwrap();
    drop(b);
    let reopened = lineage
        .reopen(&dir.path().join("b.db"), &keys[1], 3, 3)
        .await
        .unwrap();
    reopened.audit(lineage.scope("items")).await.unwrap();
    assert_eq!(
        scalar(&reopened, "SELECT count(*) FROM secure_admission_claims").await,
        2
    );
}
