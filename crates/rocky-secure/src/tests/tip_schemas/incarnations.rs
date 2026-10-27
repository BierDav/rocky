//! SQL deletes cover each live numeric incarnation at the exact business key.
use super::super::generations::{identity_insert, select, text};
use super::*;
use rocky_core::{RowApplication, SqlValue};

#[tokio::test]
async fn an_additive_delete_covers_distinct_live_ids_once_and_skips_tombstones() {
    delete_incarnations(false).await;
}

#[tokio::test]
async fn an_additive_required_default_delete_covers_distinct_live_ids_once() {
    delete_incarnations(true).await;
}

async fn delete_incarnations(defaulted: bool) {
    let lineage = Lineage::pinned(160);
    let dir = TempDir::new().unwrap();
    let keys = [
        PrivateKey::from_bytes(&[161; 32]),
        PrivateKey::from_bytes(&[162; 32]),
    ];
    let a = lineage
        .open(&dir.path().join("a.db"), &keys[0], 2, None)
        .await
        .unwrap();
    let b = lineage
        .open_certified(
            &dir.path().join("b.db"),
            (lineage.publisher(), &keys[1]),
            2,
            None,
        )
        .await
        .unwrap();
    follow_main(&lineage, &a).await;
    b.receive_tip_statements(&all_tip_statements(&a).await)
        .await
        .unwrap();
    b.follow_bookmark(lineage.domain, "main").await.unwrap();
    let sa = session(&a).await;
    let sb = session(&b).await;
    identity_insert(
        &sa,
        "INSERT INTO items(key,title,note) VALUES('same','tombstone','old')",
        vec![],
    )
    .await
    .unwrap();
    sa.run("DELETE FROM items WHERE key='same'", vec![])
        .await
        .unwrap();
    identity_insert(
        &sa,
        "INSERT INTO items(key,title,note) VALUES('same','a','a-note')",
        vec![],
    )
    .await
    .unwrap();
    identity_insert(
        &sa,
        "INSERT INTO items(key,title,note) VALUES('other','untouched','other-note')",
        vec![],
    )
    .await
    .unwrap();
    identity_insert(
        &sb,
        "INSERT INTO items(key,title,note) VALUES('same','b','b-note')",
        vec![],
    )
    .await
    .unwrap();
    let observer_key = PrivateKey::from_bytes(&[163; 32]);
    let observer = lineage
        .open(&dir.path().join("observer.db"), &observer_key, 2, None)
        .await
        .unwrap();
    for frame in stored_frames(
        &a,
        "SELECT header,body FROM secure_operations ORDER BY author,sequence",
    )
    .await
    {
        observer.receive(frame).await.unwrap();
    }
    // This honest observer has seen only A's incarnation, before the key conflict.
    let hidden_late = session(&observer)
        .await
        .run(
            "UPDATE items SET note='late-hidden-id' WHERE key='same'",
            vec![],
        )
        .await
        .unwrap()
        .committed
        .unwrap()
        .frames;
    let live_ids = [
        select(&a, "SELECT id FROM items WHERE key='same'").await[0][0].clone(),
        select(&b, "SELECT id FROM items WHERE key='same'").await[0][0].clone(),
    ];
    let late = sb
        .run(
            "UPDATE items SET note='late-old-id' WHERE key='same'",
            vec![],
        )
        .await
        .unwrap()
        .committed
        .unwrap()
        .frames;
    let b_birth = stored_frames(
        &b,
        "SELECT header,body FROM secure_operations WHERE sequence=0",
    )
    .await;
    for frame in b_birth {
        a.receive(frame).await.unwrap();
    }
    for frame in stored_frames(
        &a,
        "SELECT header,body FROM secure_operations ORDER BY author,sequence",
    )
    .await
    {
        b.receive(frame).await.unwrap();
    }
    drop(sa);
    drop(sb);
    let mut schema = lineage.app(3).schema().clone();
    if defaulted {
        let items = schema
            .tables
            .iter_mut()
            .find(|table| table.sql_name == "items")
            .unwrap();
        let rank = items
            .columns
            .iter_mut()
            .find(|column| column.sql_name == "rank")
            .unwrap();
        rank.not_null = true;
        rank.default = Some("7".into());
        items.create_sql = items
            .create_sql
            .replace("rank INTEGER", "rank INTEGER NOT NULL DEFAULT 7");
    }
    let app = rocky_core::TypedRows::new(schema.clone(), vec![lineage.domain.domain])
        .unwrap()
        .at_generation(3, BTreeSet::new())
        .unwrap();
    let source = a.followed_tip().await;
    let seal = SignedSeal::sign(
        rocky_credentials::SealStatement {
            format: rocky_credentials::SEAL_FORMAT,
            scope: lineage.domain,
            tip: source,
            frontier: a.seal_frontier(lineage.domain, false).await.unwrap(),
            action: SealAction::Continue {
                schema: app.manifest().hash(),
            },
            recovery: None,
        },
        lineage.publisher(),
    )
    .unwrap();
    let target = seal.successor().unwrap().unwrap();
    let statements = [
        TipStatement::Seal(seal),
        bookmark(&lineage, Some(source), target),
    ];
    for (replica, key, certified) in [(&a, &keys[0], false), (&b, &keys[1], true)] {
        replica.receive_tip_statements(&statements).await.unwrap();
        replica
            .follow_bookmark(lineage.domain, "main")
            .await
            .unwrap();
        replica
            .install_tip_schema(target, app.manifest(), b"verified additive".to_vec())
            .await
            .unwrap();
        let mut config = lineage.config(3, key);
        config.manifest = lineage.config(2, key).manifest;
        if certified {
            config.credentials = lineage.certified(3, lineage.publisher(), key);
        }
        let (_, domain, readers) = lineage.projection(3);
        replica
            .switch_schema(
                target,
                app.clone(),
                crate::SchemaConfig {
                    security: config,
                    tables: Some((schema.clone(), domain, readers)),
                },
            )
            .await
            .unwrap();
        assert_eq!(
            select(replica, "SELECT rank FROM items WHERE key='same'").await,
            vec![vec![if defaulted {
                SqlValue::Integer(7)
            } else {
                SqlValue::Null
            }]]
        );
    }
    let before = super::super::compaction::audit::snapshot(&a).await;
    let context = a.schema_context().await;
    let revision = a.stored_and_memory_revision().await;
    a.fail_next_sql_commit().await;
    let sa = session(&a).await;
    sa.run("BEGIN", vec![]).await.unwrap();
    sa.run("DELETE FROM items WHERE key='same'", vec![])
        .await
        .unwrap();
    assert!(matches!(
        sa.commit_publication([141; 32]).await,
        Err(Error::InjectedFailure)
    ));
    assert_eq!(super::super::compaction::audit::snapshot(&a).await, before);
    assert_eq!(a.schema_context().await, context);
    assert_eq!(a.stored_and_memory_revision().await, revision);
    sa.run("BEGIN", vec![]).await.unwrap();
    sa.run("DELETE FROM items WHERE key='same'", vec![])
        .await
        .unwrap();
    let deletion = sa
        .commit_publication([141; 32])
        .await
        .unwrap()
        .committed
        .unwrap()
        .frames;
    let rows = lineage.app(3);
    let writes = deletion
        .iter()
        .flat_map(|frame| super::staging::payload(&lineage, 3, &keys[0], frame).into_writes())
        .collect::<Vec<_>>();
    assert_eq!(writes.len(), 2);
    let mut deleted_ids = Vec::new();
    for write in &writes {
        assert!(
            write.change.before.is_some()
                && write.change.after.is_none()
                && write.assigned.is_empty()
        );
        deleted_ids.push(rows.row_identity(&write.change).unwrap().unwrap().integer());
    }
    deleted_ids.sort();
    let mut expected = live_ids
        .iter()
        .map(|value| match value {
            SqlValue::Integer(id) => *id,
            _ => panic!("numeric id"),
        })
        .collect::<Vec<_>>();
    expected.sort();
    assert_eq!(deleted_ids, expected);
    // Exact duplicate deletions and masks still fail at the real author gate.
    assert!(
        a.author_bundle(
            [142; 32],
            vec![writes[0].clone(), writes[0].clone()],
            vec![]
        )
        .await
        .is_err()
    );
    let mut invalid = writes[0].clone();
    invalid.assigned.insert(
        *invalid
            .change
            .before
            .as_ref()
            .unwrap()
            .keys()
            .next()
            .unwrap(),
    );
    assert!(
        a.author_bundle([143; 32], vec![invalid], vec![])
            .await
            .is_err()
    );
    for frame in &deletion {
        b.receive(frame.clone()).await.unwrap();
    }
    for frame in late.into_iter().chain(hidden_late) {
        a.receive(frame.clone()).await.unwrap();
        b.receive(frame).await.unwrap();
    }
    let expected = vec![vec![text("other"), text("untouched"), text("other-note")]];
    for replica in [&a, &b] {
        assert_eq!(
            select(replica, "SELECT key,title,note FROM items ORDER BY key").await,
            expected
        );
        replica
            .rebuild_projection(lineage.scope("items"))
            .await
            .unwrap();
        replica.audit(lineage.scope("items")).await.unwrap();
    }
    identity_insert(
        &sa,
        "INSERT INTO items(key,title,note) VALUES('same','replacement','fresh')",
        vec![],
    )
    .await
    .unwrap();
    assert_eq!(
        select(&a, "SELECT title,note FROM items WHERE key='same'").await,
        vec![vec![text("replacement"), text("fresh")]]
    );
    drop(sa);
    drop(a);
    let mut config = lineage.config(3, &keys[0]);
    config.manifest = lineage.config(2, &keys[0]).manifest;
    let a = EncryptedReplica::open(&dir.path().join("a.db"), app, config)
        .await
        .unwrap();
    assert_eq!(
        select(&a, "SELECT title,note FROM items WHERE key='same'").await,
        vec![vec![text("replacement"), text("fresh")]]
    );
    a.audit(lineage.scope("items")).await.unwrap();
}

#[tokio::test]
async fn deleting_a_sparse_readded_field_never_restores_its_old_value() {
    let lineage = Lineage::pinned(164);
    let dir = TempDir::new().unwrap();
    let key = PrivateKey::from_bytes(&[165; 32]);
    let path = dir.path().join("readded.db");
    let replica = lineage.open(&path, &key, 3, None).await.unwrap();
    identity_insert(
        &session(&replica).await,
        "INSERT INTO items(key,title,note) VALUES('same','source','must stay removed')",
        vec![],
    )
    .await
    .unwrap();
    let id = select(&replica, "SELECT id FROM items").await[0][0].clone();
    // Exposure makes this a retained source row, rather than a never-offered held removal.
    let peer = PrivateKey::from_bytes(&[166; 32]);
    let credentials = lineage.credentials(3, &peer);
    replica
        .admit_peer(
            peer.public_key(),
            &credentials,
            replica.followed_tip().await,
        )
        .await
        .unwrap();
    let offered = replica
        .range_page(
            peer.public_key(),
            &credentials,
            &ScopeFrontier {
                scope: lineage.scope("items"),
                logs: vec![],
            },
            None,
            MAX_SECURE_FRAME,
        )
        .await
        .unwrap();
    assert_eq!(offered.frames.len(), 1);
    for version in [4, 5] {
        lineage
            .advance(&replica, lineage.config(version, &key), version)
            .await
            .unwrap();
    }
    assert_eq!(
        select(&replica, "SELECT note FROM items").await,
        vec![vec![SqlValue::Null]]
    );
    let field = lineage.definition(5).item_id("field", "items.note");
    let records = crate::storage::rows(
        &replica
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .conn,
        "SELECT fields FROM secure_row_records",
    )
    .await
    .unwrap();
    assert_eq!(records.len(), 1);
    let [SqlValue::Blob(bytes)] = records[0].as_slice() else {
        panic!("materialized row")
    };
    #[derive(serde::Deserialize)]
    struct StoredFields {
        fields: std::collections::BTreeMap<serde_bytes::ByteArray<32>, serde_bytes::ByteBuf>,
    }
    let canonical = encode(
        &replica
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .decode_record(bytes)
            .await
            .unwrap(),
    )
    .unwrap();
    let record: StoredFields = p2panda_core::cbor::decode_cbor(canonical.as_slice()).unwrap();
    assert!(record.fields.keys().all(|key| key.as_ref() != &field));
    let frames = session(&replica)
        .await
        .run("DELETE FROM items WHERE key='same'", vec![])
        .await
        .unwrap()
        .committed
        .unwrap()
        .frames;
    let writes = frames
        .iter()
        .flat_map(|frame| super::staging::payload(&lineage, 5, &key, frame).into_writes())
        .collect::<Vec<_>>();
    assert_eq!(writes.len(), 1);
    assert!(writes[0].change.after.is_none() && writes[0].assigned.is_empty());
    assert_eq!(
        SqlValue::Integer(
            lineage
                .app(5)
                .row_identity(&writes[0].change)
                .unwrap()
                .unwrap()
                .integer()
        ),
        id
    );
    let mut null = Vec::new();
    SqlValue::Null.encode(&mut null);
    assert_eq!(
        writes[0].change.before.as_ref().unwrap().get(&field),
        Some(&null)
    );
    replica
        .rebuild_projection(lineage.scope("items"))
        .await
        .unwrap();
    assert!(select(&replica, "SELECT * FROM items").await.is_empty());
    replica.audit(lineage.scope("items")).await.unwrap();
    drop(replica);
    let replica = lineage.reopen(&path, &key, 3, 5).await.unwrap();
    assert!(select(&replica, "SELECT * FROM items").await.is_empty());
    replica.audit(lineage.scope("items")).await.unwrap();
}
