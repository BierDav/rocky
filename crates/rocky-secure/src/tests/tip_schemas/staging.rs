//! Final admission binds the frozen parent to the original signed frontier.
use super::*;
use crate::{LineNode, SchemaConfig, SchemaStage};
use rocky_core::{RowApplication, RowIdentity};
use rocky_credentials::{SignedMigrationPublication, SignedTipOpening, TipOpeningStatement};

async fn candidate(lineage: &Lineage, replica: &super::super::compaction::Replica) -> SchemaStage {
    candidate_at(lineage, replica, 2, 3).await
}

async fn candidate_at(
    lineage: &Lineage,
    replica: &super::super::compaction::Replica,
    source_version: u32,
    target_version: u32,
) -> SchemaStage {
    replica
        .publish_tip_statements(&[TipStatement::GenesisLine(
            rocky_credentials::SignedGenesisLine::sign(
                rocky_credentials::GenesisLineStatement {
                    format: 1,
                    scope: lineage.domain,
                    minimum_in_sync: 2,
                },
                lineage.publisher(),
            )
            .unwrap(),
        )])
        .await
        .unwrap();
    let source = replica.followed_tip().await;
    let close = SignedSeal::sign(
        rocky_credentials::SealStatement {
            format: rocky_credentials::SEAL_FORMAT,
            scope: lineage.domain,
            tip: source,
            frontier: replica.seal_frontier(lineage.domain, true).await.unwrap(),
            action: SealAction::Close,
            recovery: None,
        },
        lineage.publisher(),
    )
    .unwrap();
    let opening = SignedTipOpening::sign(
        TipOpeningStatement {
            format: 2,
            scope: lineage.domain,
            seal: close.id().unwrap(),
            schema: lineage.definition(target_version).hash(),
            minimum_in_sync: 2,
        },
        lineage.publisher(),
    )
    .unwrap();
    let target = opening.tip().unwrap();
    SchemaStage {
        publication: rocky_core::commitment("rocky.sdk.migration.v3", &target.0),
        scope: lineage.domain,
        bookmark: "main".into(),
        source,
        target,
        from_version: source_version,
        coordinated: false,
        statements: vec![
            TipStatement::Seal(close),
            TipStatement::Opening(opening),
            bookmark(lineage, Some(source), target),
        ],
        program: vec![],
        parents: vec!["items".into()],
    }
}

async fn graph(replica: &super::super::compaction::Replica, lineage: &Lineage) -> Vec<u8> {
    encode(&replica.tip_graph(lineage.domain).await.unwrap()).unwrap()
}

#[tokio::test]
async fn a_source_write_between_preflight_and_admission_refuses_the_stage_atomically() {
    migration_race(2, 3, false).await;
}

#[tokio::test]
async fn an_additive_migration_deletes_the_visible_incarnation_once() {
    migration_race(2, 3, true).await;
}

#[tokio::test]
async fn a_removing_migration_holds_complete_source_images() {
    migration_race(3, 4, true).await;
}

async fn migration_race(source_version: u32, target_version: u32, delete: bool) {
    let lineage = Lineage::new();
    let dir = TempDir::new().unwrap();
    let key = PrivateKey::new();
    let a = lineage
        .open(&dir.path().join("a.db"), &key, source_version, None)
        .await
        .unwrap();
    follow_main(&lineage, &a).await;
    insert(&a, "one", "sealed parent").await;
    let publisher_key = PrivateKey::new();
    let publisher = lineage
        .open_certified(
            &dir.path().join("publisher.db"),
            (lineage.publisher(), &publisher_key),
            source_version,
            None,
        )
        .await
        .unwrap();
    publisher
        .receive_tip_statements(&all_tip_statements(&a).await)
        .await
        .unwrap();
    publisher
        .follow_bookmark(lineage.domain, "main")
        .await
        .unwrap();
    for result in publisher
        .receive_all(stored_frames(&a, "SELECT header,body FROM secure_operations").await)
        .await
    {
        result.unwrap();
    }
    let stage = candidate_at(&lineage, &a, source_version, target_version).await;
    publisher
        .receive_tip_statements(&all_tip_statements(&a).await)
        .await
        .unwrap();
    let signed_bytes = encode(&stage).unwrap();
    let (ready, resume) = a.pause_schema_staging().await;
    let mut staging = {
        let a = a.clone();
        let stage = stage.clone();
        tokio::spawn(async move { a.stage_schema(stage).await })
    };
    tokio::select! {
        ready = ready => ready.unwrap(),
        result = &mut staging => panic!("stage completed before preflight pause: {result:?}"),
    }
    // Another task commits through ordinary SQL while staging is outside its guard.
    let write = super::super::generations::identity_insert(
        &session(&a).await,
        "INSERT INTO items(key,title,note) VALUES('late','beyond the signed close','original non-NULL source note')",
        vec![],
    )
    .await
    .unwrap()
    .committed
    .unwrap()
    .frames;
    assert_eq!(write.len(), 1);
    let original = write[0].clone();
    let applied = applied_binding(&a, &original).await;
    assert_eq!(applied.len(), 1);
    assert_ne!(
        a.seal_frontier(lineage.domain, true).await.unwrap(),
        match &stage.statements[0] {
            TipStatement::Seal(seal) => seal.statement.frontier.clone(),
            _ => unreachable!(),
        }
    );
    let before = super::super::compaction::audit::snapshot(&a).await;
    let before_graph = graph(&a, &lineage).await;
    let context = a.schema_context().await;
    let revision = a.stored_and_memory_revision().await;
    resume.send(()).unwrap();
    let error = staging.await.unwrap().unwrap_err();
    assert!(
        error
            .to_string()
            .contains("source frontier changed before schema staging"),
        "{error}"
    );
    assert_eq!(super::super::compaction::audit::snapshot(&a).await, before);
    assert_eq!(graph(&a, &lineage).await, before_graph);
    assert_eq!(a.schema_context().await, context);
    assert_eq!(a.stored_and_memory_revision().await, revision);
    assert!(a.staged_schema().await.unwrap().is_none());
    assert_eq!(encode(&stage).unwrap(), signed_bytes);

    // Serve the genuine late frame so normal following retains its original bytes.
    let peer = PrivateKey::new();
    let credentials = lineage.credentials(source_version, &peer);
    a.admit_peer(peer.public_key(), &credentials, stage.source)
        .await
        .unwrap();
    let offered = a
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
    assert!(
        offered
            .frames
            .iter()
            .any(|frame| frame.id().unwrap() == original.id().unwrap())
    );

    // The other publisher still has exactly F and takes the normal migration path.
    publisher.stage_schema(stage.clone()).await.unwrap();
    assert_eq!(
        encode(&publisher.staged_schema().await.unwrap().unwrap().0).unwrap(),
        signed_bytes
    );
    migrate_at(
        &lineage,
        &publisher,
        &publisher_key,
        &stage,
        target_version,
        delete,
    )
    .await;
    if source_version == 3 {
        a.inject_plan_fault(crate::replica::migrate::PlanStage::Switch, 0)
            .await;
        assert!(matches!(
            lineage
                .follow(
                    &a,
                    lineage.config(target_version, &key),
                    target_version,
                    &all_tip_statements(&publisher).await
                )
                .await,
            Err(Error::InjectedFailure)
        ));
        let before = super::super::compaction::audit::snapshot(&a).await;
        let before_graph = graph(&a, &lineage).await;
        let context = a.schema_context().await;
        let revision = a.stored_and_memory_revision().await;
        a.inner.lock().await.conn.execute("CREATE TEMP TRIGGER fail_held_switch AFTER INSERT ON secure_held_frames BEGIN SELECT RAISE(ABORT,'held switch rollback'); END", ()).await.unwrap();
        let error = lineage
            .follow(
                &a,
                lineage.config(target_version, &key),
                target_version,
                &all_tip_statements(&publisher).await,
            )
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("held switch rollback"),
            "{error}"
        );
        assert_eq!(super::super::compaction::audit::snapshot(&a).await, before);
        assert_eq!(graph(&a, &lineage).await, before_graph);
        assert_eq!(a.schema_context().await, context);
        assert_eq!(a.stored_and_memory_revision().await, revision);
        a.inner
            .lock()
            .await
            .conn
            .execute("DROP TRIGGER fail_held_switch", ())
            .await
            .unwrap();
    }
    lineage
        .follow(
            &a,
            lineage.config(target_version, &key),
            target_version,
            &all_tip_statements(&publisher).await,
        )
        .await
        .unwrap();
    let frames = stored_frames(
        &publisher,
        "SELECT header,body FROM secure_operations ORDER BY sequence",
    )
    .await;
    let migration = frames
        .into_iter()
        .filter(|frame| frame.migration.is_some())
        .collect::<Vec<_>>();
    assert!(!migration.is_empty());
    for result in a.receive_migration_all(migration).await {
        result.unwrap();
    }
    let mut expected = vec![
        vec![
            super::super::generations::text("copy-one"),
            super::super::generations::text("sealed parent"),
        ],
        vec![
            super::super::generations::text("one"),
            super::super::generations::text("sealed parent"),
        ],
    ];
    if delete {
        expected.pop();
    }
    assert_eq!(
        super::super::generations::select(&publisher, "SELECT key,title FROM items ORDER BY key")
            .await,
        expected
    );
    assert_eq!(
        super::super::generations::select(&a, "SELECT key,title FROM items ORDER BY key").await,
        expected
    );
    assert_eq!(a.followed_tip().await, stage.target);
    for replica in [&a, &publisher] {
        assert_eq!(
            replica.inner.lock().await.app.manifest().hash(),
            lineage.definition(target_version).hash()
        );
    }
    assert_held(&a, &original, &applied).await;
    let original_writes = payload(&lineage, source_version, &key, &original).into_writes();
    assert_eq!(original_writes.len(), 1);
    assert!(original_writes[0].assigned.is_empty());
    let definition = lineage.definition(source_version);
    let note = definition.item_id("field", "items.note");
    let mut bytes = Vec::new();
    rocky_core::SqlValue::Text("original non-NULL source note".into()).encode(&mut bytes);
    assert_eq!(
        original_writes[0].change.after.as_ref().unwrap().get(&note),
        Some(&bytes)
    );
    let held = a.held_changes().await.unwrap();
    assert_eq!(held[0].rows[0].table, "items");
    let original_id = lineage
        .app(source_version)
        .row_identity(&original_writes[0].change)
        .unwrap()
        .unwrap();
    assert_eq!(
        held[0].rows[0].row,
        rocky_core::commitment(
            "rocky.row-incarnation.v2",
            &original_id.integer().to_le_bytes()
        )
    );
    let note_field = held[0].rows[0]
        .fields
        .iter()
        .find(|field| field.field == note)
        .unwrap();
    assert_eq!(note_field.value, bytes);
    assert!(note_field.written);
    assert!(
        held[0].rows[0]
            .fields
            .iter()
            .any(|field| field.name == "note")
    );
    a.audit(lineage.scope("items")).await.unwrap();
    publisher.audit(lineage.scope("items")).await.unwrap();
    drop(a);
    let mut security = lineage.config(source_version, &key);
    security.manifest = lineage.config(source_version, &key).manifest;
    let a = EncryptedReplica::open(
        &dir.path().join("a.db"),
        lineage.app(target_version),
        security,
    )
    .await
    .unwrap();
    assert_eq!(
        super::super::generations::select(&a, "SELECT key,title FROM items ORDER BY key").await,
        expected
    );
    assert_held(&a, &original, &applied).await;
    let (page, next) = a
        .held_frame_page(
            peer.public_key(),
            &credentials,
            lineage.scope("items"),
            None,
            MAX_SECURE_FRAME,
        )
        .await
        .unwrap();
    assert!(next.is_none());
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].header_bytes(), original.header_bytes());
    assert_eq!(page[0].body_bytes(), original.body_bytes());
    assert_eq!(
        payload(&lineage, source_version, &key, &page[0]).writes(),
        original_writes
    );
    a.audit(lineage.scope("items")).await.unwrap();
    if source_version == 3 {
        let extra_key = PrivateKey::new();
        let extra = lineage
            .open_certified(
                &dir.path().join("extra.db"),
                (lineage.publisher(), &extra_key),
                3,
                None,
            )
            .await
            .unwrap();
        // This source node legitimately authored a bundle without learning the close.
        let sql = extra
            .session([lineage.scope("items"), lineage.scope("tags")].into())
            .await
            .unwrap()
            .sql(crate::sql::SqlOptions::default())
            .await
            .unwrap();
        sql.run("BEGIN", vec![]).await.unwrap();
        super::super::generations::identity_insert(
            &sql,
            "INSERT INTO items(key,title,note) VALUES('extra','held source','extra note')",
            vec![],
        )
        .await
        .unwrap();
        super::super::generations::identity_insert(
            &sql,
            "INSERT INTO tags(key,label) VALUES('extra-tag','removed source table')",
            vec![],
        )
        .await
        .unwrap();
        let frames = sql
            .commit([144; 32])
            .await
            .unwrap()
            .committed
            .unwrap()
            .frames;
        assert_eq!(frames.len(), 2);
        a.receive_held_frames(frames.clone()).await.unwrap();
        let changes = a.held_changes().await.unwrap();
        assert_eq!(changes.len(), 2);
        let bundle = changes
            .iter()
            .find(|change| change.rows.len() == 2)
            .unwrap();
        assert!(
            bundle
                .rows
                .iter()
                .any(|row| row.table == "tags" && row.removed)
        );
        assert!(
            bundle
                .rows
                .iter()
                .any(|row| row.table == "items" && !row.removed)
        );
        for scope in [lineage.scope("items"), lineage.scope("tags")] {
            let (page, _) = a
                .held_frame_page(
                    peer.public_key(),
                    &credentials,
                    scope,
                    None,
                    MAX_SECURE_FRAME,
                )
                .await
                .unwrap();
            for original in frames
                .iter()
                .filter(|frame| frame.scope().unwrap() == scope)
            {
                let stored = page
                    .iter()
                    .find(|frame| frame.id().unwrap() == original.id().unwrap())
                    .unwrap();
                assert_eq!(stored.header_bytes(), original.header_bytes());
                assert_eq!(stored.body_bytes(), original.body_bytes());
            }
            a.audit(scope).await.unwrap();
        }
        assert_eq!(
            super::super::generations::select(&a, "SELECT key,title FROM items ORDER BY key").await,
            expected
        );
    }
}

async fn migrate_at(
    lineage: &Lineage,
    replica: &super::super::compaction::Replica,
    key: &PrivateKey,
    stage: &SchemaStage,
    target_version: u32,
    delete: bool,
) {
    replica
        .install_tip_schema(
            stage.target,
            lineage.definition(target_version),
            b"verified".to_vec(),
        )
        .await
        .unwrap();
    let mut security = lineage.config(target_version, key);
    security.manifest = lineage.config(stage.from_version, key).manifest;
    security.credentials = lineage.certified(target_version, lineage.publisher(), key);
    replica
        .switch_schema(
            stage.target,
            lineage.app(target_version),
            SchemaConfig {
                security,
                tables: Some(lineage.projection(target_version)),
            },
        )
        .await
        .unwrap();
    let writer = PrivateKey::new();
    let credentials = lineage.certified(target_version, lineage.publisher(), &writer);
    let writer_key = writer.clone();
    let author = credentials.author_number().unwrap();
    replica
        .register_line_node(LineNode {
            key: writer,
            credentials,
        })
        .await
        .unwrap();
    let sql = replica
        .session([lineage.scope("items")].into())
        .await
        .unwrap()
        .migration_sql(stage.publication, crate::sql::SqlOptions::default())
        .await
        .unwrap();
    sql.run("BEGIN", vec![]).await.unwrap();
    assert_eq!(
        sql.run("SELECT key,title FROM parent.items ORDER BY key", vec![])
            .await
            .unwrap()
            .rows,
        vec![vec![
            super::super::generations::text("one"),
            super::super::generations::text("sealed parent")
        ]]
    );
    let old_id = sql
        .run("SELECT id FROM parent.items", vec![])
        .await
        .unwrap()
        .rows[0][0]
        .clone();
    if delete {
        sql.run("DELETE FROM items", vec![]).await.unwrap();
    }
    sql.run(
        "INSERT INTO items(key,title,id) SELECT 'copy-'||key,title,? FROM parent.items",
        vec![rocky_core::SqlValue::Integer(
            RowIdentity::new(author, 1).unwrap().integer(),
        )],
    )
    .await
    .unwrap();
    let committed = sql
        .commit_publication(stage.publication)
        .await
        .unwrap()
        .committed
        .unwrap();
    let writes = committed
        .frames
        .iter()
        .flat_map(|frame| payload(lineage, target_version, &writer_key, frame).into_writes())
        .collect::<Vec<_>>();
    let deleted = writes
        .iter()
        .filter(|write| write.change.after.is_none())
        .collect::<Vec<_>>();
    assert_eq!(deleted.len(), usize::from(delete));
    if delete {
        assert!(deleted[0].assigned.is_empty());
        assert_eq!(
            rocky_core::SqlValue::Integer(
                lineage
                    .app(target_version)
                    .row_identity(&deleted[0].change)
                    .unwrap()
                    .unwrap()
                    .integer()
            ),
            old_id
        );
    }
    let born = writes
        .iter()
        .filter(|write| write.change.before.is_none())
        .collect::<Vec<_>>();
    assert_eq!(born.len(), 1);
    let new_id = lineage
        .app(target_version)
        .row_identity(&born[0].change)
        .unwrap()
        .unwrap();
    assert_eq!(new_id, RowIdentity::new(author, 1).unwrap());
    assert_ne!(rocky_core::SqlValue::Integer(new_id.integer()), old_id);
    drop(sql);
    let (statement, writer) = replica
        .migration_intent(stage.publication)
        .await
        .unwrap()
        .unwrap();
    let signed = SignedMigrationPublication::endorse(
        statement,
        lineage.publisher(),
        &rocky_credentials::CredentialChain::default(),
        writer,
    )
    .unwrap();
    replica.admit_migration_publication(signed).await.unwrap();
    replica
        .release_uncoordinated_schema(stage.publication)
        .await
        .unwrap();
}

async fn applied_binding(
    replica: &super::super::compaction::Replica,
    original: &EncryptedFrame,
) -> Vec<Vec<rocky_core::SqlValue>> {
    crate::storage::collect_rows(
        replica
            .inner
            .lock()
            .await
            .conn
            .query(
                "SELECT scope,hash FROM secure_applied WHERE hash=?",
                [original.id().unwrap().as_bytes().to_vec()],
            )
            .await
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn assert_held(
    replica: &super::super::compaction::Replica,
    original: &EncryptedFrame,
    applied: &[Vec<rocky_core::SqlValue>],
) {
    assert_eq!(replica.held_changes().await.unwrap().len(), 1);
    assert_eq!(applied_binding(replica, original).await, applied);
    let inner = replica.inner.lock().await;
    assert!(
        crate::storage::rows(&inner.conn, "SELECT * FROM secure_held_resolved")
            .await
            .unwrap()
            .is_empty()
    );
    let rows = crate::storage::collect_rows(inner.conn.query(
        "SELECT o.header,o.body,o.status,(SELECT count(*) FROM secure_applied a WHERE a.hash=o.hash),(SELECT count(*) FROM secure_voids v WHERE v.hash=o.hash),h.frame FROM secure_operations o JOIN secure_held_frames h ON h.hash=o.hash WHERE o.hash=?",
        [original.id().unwrap().as_bytes().to_vec()],
    ).await.unwrap()).await.unwrap();
    let [
        rocky_core::SqlValue::Blob(header),
        rocky_core::SqlValue::Blob(body),
        rocky_core::SqlValue::Text(status),
        rocky_core::SqlValue::Integer(applied),
        rocky_core::SqlValue::Integer(voids),
        rocky_core::SqlValue::Blob(held),
    ] = rows[0].as_slice()
    else {
        panic!("held original")
    };
    assert_eq!(header, original.header_bytes());
    assert_eq!(body, original.body_bytes());
    assert_eq!((status.as_str(), *applied, *voids), ("held", 1, 0));
    let held: EncryptedFrame = decode(held).unwrap();
    assert_eq!(held.header_bytes(), original.header_bytes());
    assert_eq!(held.body_bytes(), original.body_bytes());
}

#[tokio::test]
async fn racing_stages_recheck_private_state_before_admission() {
    let lineage = Lineage::new();
    let dir = TempDir::new().unwrap();
    let key = PrivateKey::new();
    let a = lineage
        .open(&dir.path().join("a.db"), &key, 2, None)
        .await
        .unwrap();
    follow_main(&lineage, &a).await;
    let stage = candidate(&lineage, &a).await;
    let (ready, resume) = a.pause_schema_staging().await;
    let mut first = {
        let a = a.clone();
        let stage = stage.clone();
        tokio::spawn(async move { a.stage_schema(stage).await })
    };
    tokio::select! {
        ready = ready => ready.unwrap(),
        result = &mut first => panic!("stage completed before preflight pause: {result:?}"),
    }
    a.stage_schema(stage.clone()).await.unwrap();
    let before = super::super::compaction::audit::snapshot(&a).await;
    let before_graph = graph(&a, &lineage).await;
    let context = a.schema_context().await;
    resume.send(()).unwrap();
    let error = first.await.unwrap().unwrap_err();
    assert!(
        error
            .to_string()
            .contains("a schema publication is already pending"),
        "{error}"
    );
    assert_eq!(super::super::compaction::audit::snapshot(&a).await, before);
    assert_eq!(graph(&a, &lineage).await, before_graph);
    assert_eq!(a.schema_context().await, context);
    assert_eq!(
        a.staged_schema().await.unwrap().unwrap().0.publication,
        stage.publication
    );
    a.withdraw_staged_schema(
        stage.publication,
        lineage.app(2),
        SchemaConfig {
            security: lineage.config(2, &key),
            tables: Some(lineage.projection(2)),
        },
    )
    .await
    .unwrap();
    a.audit(lineage.scope("items")).await.unwrap();
}

#[tokio::test]
async fn a_source_move_between_preflight_and_admission_refuses_the_stage_atomically() {
    let lineage = Lineage::new();
    let dir = TempDir::new().unwrap();
    let key = PrivateKey::new();
    let a = lineage
        .open(&dir.path().join("a.db"), &key, 2, None)
        .await
        .unwrap();
    follow_main(&lineage, &a).await;
    let stage = candidate(&lineage, &a).await;
    let (ready, resume) = a.pause_schema_staging().await;
    let mut staging = {
        let a = a.clone();
        let stage = stage.clone();
        tokio::spawn(async move { a.stage_schema(stage).await })
    };
    tokio::select! {
        ready = ready => ready.unwrap(),
        result = &mut staging => panic!("stage completed before preflight pause: {result:?}"),
    }
    let seal = SignedSeal::sign(
        rocky_credentials::SealStatement {
            format: rocky_credentials::SEAL_FORMAT,
            scope: lineage.domain,
            tip: stage.source,
            frontier: a.seal_frontier(lineage.domain, false).await.unwrap(),
            action: SealAction::Continue {
                schema: lineage.definition(2).hash(),
            },
            recovery: None,
        },
        lineage.publisher(),
    )
    .unwrap();
    let target = seal.successor().unwrap().unwrap();
    a.publish_tip_statements(&[
        TipStatement::Seal(seal),
        bookmark(&lineage, Some(stage.source), target),
    ])
    .await
    .unwrap();
    assert_eq!(a.followed_tip().await, target);
    let before = super::super::compaction::audit::snapshot(&a).await;
    let before_graph = graph(&a, &lineage).await;
    let context = a.schema_context().await;
    resume.send(()).unwrap();
    let error = staging.await.unwrap().unwrap_err();
    assert!(
        error
            .to_string()
            .contains("schema publication must leave its followed open tip"),
        "{error}"
    );
    assert_eq!(super::super::compaction::audit::snapshot(&a).await, before);
    assert_eq!(graph(&a, &lineage).await, before_graph);
    assert_eq!(a.schema_context().await, context);
    assert!(a.staged_schema().await.unwrap().is_none());
    a.audit(lineage.scope("items")).await.unwrap();
}

pub(super) fn payload(
    lineage: &Lineage,
    generation: u32,
    node: &PrivateKey,
    frame: &EncryptedFrame,
) -> crate::wire::RowPayload {
    let config = lineage.config(generation, node);
    let operation = frame.operation_ref().unwrap();
    let key = config
        .keys
        .iter()
        .find(|key| key.scope == operation.header.extensions.scope)
        .unwrap();
    let bytes = crate::wire::decrypt(
        &key.key,
        &encode(&operation.header.extensions).unwrap(),
        frame.body_bytes(),
    )
    .unwrap();
    crate::wire::RowPayload::decode(&bytes).unwrap()
}
