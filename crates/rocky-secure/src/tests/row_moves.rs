use super::*;

const TABLES: &[&str] = &["lists", "categories", "items", "attachments"];
fn rights() -> [Right; 4] {
    [Right::Read, Right::Insert, Right::Update, Right::Delete]
}
async fn phone(f: &Fixture, path: &Path, author: u32, held: &[i64]) -> EncryptedReplica<TypedRows> {
    let replica = f
        .open(
            path,
            PrivateKey::new(),
            f.grants(None, &rights()),
            Some((PrivateKey::new(), author)),
        )
        .await;
    for partition in held {
        f.keys(&replica, *partition, TABLES).await;
    }
    replica
}
async fn session(
    f: &Fixture,
    phone: &EncryptedReplica<TypedRows>,
    held: &[i64],
) -> crate::SecureSession<TypedRows> {
    phone
        .session(
            held.iter()
                .flat_map(|partition| TABLES.iter().map(move |name| f.scope(name, *partition)))
                .collect(),
        )
        .await
        .unwrap()
}
async fn send(frames: &[EncryptedFrame], to: &EncryptedReplica<TypedRows>) {
    for frame in frames {
        to.receive(frame.clone()).await.unwrap();
    }
}
async fn setup(
    f: &Fixture,
    a: &EncryptedReplica<TypedRows>,
    b: &EncryptedReplica<TypedRows>,
    partitions: &[i64],
) -> RowWrite {
    for (index, partition) in partitions.iter().enumerate() {
        let frames = a
            .author_bundle(
                [index as u8 + 1; 32],
                vec![f.write("lists", *partition, index as u32 + 1, None)],
                vec![],
            )
            .await
            .unwrap();
        send(&frames, b).await;
    }
    let row = f.write("items", partitions[0], 1, None);
    let frames = a
        .author_bundle([10; 32], vec![row.clone()], vec![])
        .await
        .unwrap();
    send(&frames, b).await;
    row
}
async fn edit(
    f: &Fixture,
    phone: &EncryptedReplica<TypedRows>,
    row: &RowWrite,
    title: &str,
    handle: u8,
) -> Vec<EncryptedFrame> {
    let typed = f.app();
    let table = typed
        .tables()
        .find(|table| table.sql_name == "items")
        .unwrap();
    let logical = f.schema.table("items").unwrap();
    let field = f.schema.field_id(logical, logical.column("title").unwrap());
    let mut after = row.change.after.clone().unwrap();
    let mut value = Vec::new();
    SqlValue::Text(title.into()).encode(&mut value);
    after.insert(field, value);
    let mut change = row.change.clone();
    change.before = change.after.take();
    change.after = Some(after);
    assert_eq!(typed.row_identity(&change).unwrap().unwrap().author(), 1);
    assert!(table.fields.contains(&field));
    phone
        .author_bundle([handle; 32], vec![RowWrite::changed(change)], vec![])
        .await
        .unwrap()
}
async fn title(f: &Fixture, phone: &EncryptedReplica<TypedRows>, partition: i64) -> Option<String> {
    let snapshot = phone.snapshot(f.scope("items", partition)).await.unwrap();
    let typed = f.app();
    let table = typed
        .tables()
        .find(|table| table.sql_name == "items")
        .unwrap();
    let position = f
        .schema
        .table("items")
        .unwrap()
        .columns
        .iter()
        .position(|column| column.sql_name == "title")
        .unwrap();
    snapshot.rows.values().next().map(|fields| {
        match &typed.values(table, fields).unwrap()[position] {
            SqlValue::Text(value) => value.clone(),
            _ => panic!("title"),
        }
    })
}

#[tokio::test]
async fn foreign_author_move_follows_offline_edit_once_and_keeps_identity() {
    let f = Fixture::new();
    let root = tempfile::tempdir().unwrap();
    let partitions = [
        RowIdentity::new(1, 1).unwrap().integer(),
        RowIdentity::new(1, 2).unwrap().integer(),
    ];
    let a = phone(&f, &root.path().join("a"), 1, &partitions).await;
    let b = phone(&f, &root.path().join("b"), 2, &partitions).await;
    let row = setup(&f, &a, &b, &partitions).await;
    let offline = edit(&f, &a, &row, "ticked", 20).await;
    let mover = session(&f, &b, &partitions).await;
    let frames = mover
        .move_row(
            [21; 32],
            "items",
            RowIdentity::new(1, 1).unwrap(),
            &[SqlValue::Integer(partitions[0])],
            &[SqlValue::Integer(partitions[1])],
        )
        .await
        .unwrap();
    assert_eq!(frames.len(), 2);
    assert_eq!(title(&f, &b, partitions[0]).await, None);
    assert_eq!(title(&f, &b, partitions[1]).await.as_deref(), Some("Item"));
    // Either first member leaves the old row visible until the partner arrives.
    a.receive(frames[0].clone()).await.unwrap();
    assert_eq!(
        title(&f, &a, partitions[0]).await.as_deref(),
        Some("ticked")
    );
    assert_eq!(title(&f, &a, partitions[1]).await, None);
    a.receive(frames[1].clone()).await.unwrap();
    assert_eq!(title(&f, &a, partitions[0]).await, None);
    assert_eq!(
        title(&f, &a, partitions[1]).await.as_deref(),
        Some("ticked")
    );
    send(&offline, &b).await;
    let outgoing = crate::tests::stored_frames(
        &a,
        "SELECT header,body FROM secure_operations ORDER BY sequence",
    )
    .await;
    send(&outgoing, &b).await;
    assert_eq!(
        title(&f, &b, partitions[1]).await.as_deref(),
        Some("ticked")
    );
    let before = crate::storage::rows(&a.inner.lock().await.conn, "SELECT * FROM secure_followed")
        .await
        .unwrap();
    assert_eq!(before.len(), 1);
    send(&frames, &a).await;
    let after = crate::storage::rows(&a.inner.lock().await.conn, "SELECT * FROM secure_followed")
        .await
        .unwrap();
    assert_eq!(before, after);
    for partition in partitions {
        a.audit(f.scope("items", partition)).await.unwrap();
        b.audit(f.scope("items", partition)).await.unwrap();
    }
}

#[tokio::test]
async fn source_only_author_drops_offline_update_and_cannot_open_destination() {
    let f = Fixture::new();
    let root = tempfile::tempdir().unwrap();
    let partitions = [
        RowIdentity::new(1, 1).unwrap().integer(),
        RowIdentity::new(1, 2).unwrap().integer(),
    ];
    let a = phone(&f, &root.path().join("a"), 1, &partitions).await;
    let b = phone(&f, &root.path().join("b"), 2, &partitions).await;
    let row = setup(&f, &a, &b, &partitions).await;
    let offline = edit(&f, &a, &row, "ticked", 30).await;
    a.evict_key(f.scope("items", partitions[1])).await;
    let frames = session(&f, &b, &partitions)
        .await
        .move_row(
            [31; 32],
            "items",
            RowIdentity::new(1, 1).unwrap(),
            &[SqlValue::Integer(partitions[0])],
            &[SqlValue::Integer(partitions[1])],
        )
        .await
        .unwrap();
    let deletion = frames
        .iter()
        .find(|frame| frame.scope().unwrap() == f.scope("items", partitions[0]))
        .unwrap();
    let insertion = frames
        .iter()
        .find(|frame| frame.scope().unwrap() == f.scope("items", partitions[1]))
        .unwrap();
    a.receive(deletion.clone()).await.unwrap();
    assert_eq!(title(&f, &a, partitions[0]).await, None);
    assert!(payload(&a, insertion).await.is_err());
    send(&offline, &b).await;
    assert_eq!(title(&f, &b, partitions[1]).await.as_deref(), Some("Item"));
    assert!(
        crate::storage::rows(&a.inner.lock().await.conn, "SELECT * FROM secure_followed")
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn destination_edit_with_later_stamp_wins_over_offline_update() {
    let f = Fixture::new();
    let root = tempfile::tempdir().unwrap();
    let partitions = [
        RowIdentity::new(1, 1).unwrap().integer(),
        RowIdentity::new(1, 2).unwrap().integer(),
    ];
    let a = phone(&f, &root.path().join("a"), 1, &partitions).await;
    let b = phone(&f, &root.path().join("b"), 2, &partitions).await;
    let row = setup(&f, &a, &b, &partitions).await;
    let offline = edit(&f, &a, &row, "ticked", 40).await;
    let frames = session(&f, &b, &partitions)
        .await
        .move_row(
            [41; 32],
            "items",
            RowIdentity::new(1, 1).unwrap(),
            &[SqlValue::Integer(partitions[0])],
            &[SqlValue::Integer(partitions[1])],
        )
        .await
        .unwrap();
    let destination = payload(
        &b,
        frames
            .iter()
            .find(|frame| frame.scope().unwrap() == f.scope("items", partitions[1]))
            .unwrap(),
    )
    .await
    .unwrap()
    .into_writes()
    .remove(0);
    let later = edit(&f, &b, &destination, "later", 42).await;
    // Store the B edit first: it waits for its creation, then the move admits it.
    send(&later, &a).await;
    send(&frames, &a).await;
    send(&offline, &b).await;
    assert_eq!(title(&f, &a, partitions[1]).await.as_deref(), Some("later"));
    assert_eq!(title(&f, &b, partitions[1]).await.as_deref(), Some("later"));
}

async fn add_related(
    f: &Fixture,
    a: &EncryptedReplica<TypedRows>,
    b: &EncryptedReplica<TypedRows>,
    partition: i64,
) {
    let category = f.write("categories", partition, 1, None);
    send(
        &a.author_bundle([50; 32], vec![category], vec![])
            .await
            .unwrap(),
        b,
    )
    .await;
    let typed = f.app();
    let table = typed
        .tables()
        .find(|table| table.sql_name == "attachments")
        .unwrap();
    let values = vec![
        SqlValue::Integer(partition),
        SqlValue::Integer(RowIdentity::new(1, 1).unwrap().integer()),
        SqlValue::Text("file pointer".into()),
        SqlValue::Integer(RowIdentity::new(1, 1).unwrap().integer()),
        SqlValue::Integer(0),
    ];
    let write = RowWrite::changed(RowChange {
        scope: f.scope("attachments", partition),
        row: typed.row_id(f.domain.domain, table, &values).unwrap(),
        before: None,
        after: Some(typed.fields(table, &values).unwrap()),
    });
    send(
        &a.author_bundle([51; 32], vec![write], vec![])
            .await
            .unwrap(),
        b,
    )
    .await;
}

#[tokio::test]
async fn attachments_move_in_same_bundle_category_nulls_and_b_only_reader_rebuilds() {
    let f = Fixture::new();
    let root = tempfile::tempdir().unwrap();
    let partitions = [
        RowIdentity::new(1, 1).unwrap().integer(),
        RowIdentity::new(1, 2).unwrap().integer(),
    ];
    let a = phone(&f, &root.path().join("a"), 1, &partitions).await;
    let b = phone(&f, &root.path().join("b"), 2, &partitions).await;
    let row = setup(&f, &a, &b, &partitions).await;
    add_related(&f, &a, &b, partitions[0]).await;
    let mut change = row.change.clone();
    change.before = change.after.take();
    let mut after = change.before.clone().unwrap();
    let category = f.schema.table("items").unwrap().column("category").unwrap();
    let field = f
        .schema
        .field_id(f.schema.table("items").unwrap(), category);
    let mut value = Vec::new();
    SqlValue::Integer(RowIdentity::new(1, 1).unwrap().integer()).encode(&mut value);
    after.insert(field, value);
    change.after = Some(after);
    send(
        &a.author_bundle([52; 32], vec![RowWrite::changed(change)], vec![])
            .await
            .unwrap(),
        &b,
    )
    .await;
    let frames = session(&f, &b, &partitions)
        .await
        .move_row(
            [53; 32],
            "items",
            RowIdentity::new(1, 1).unwrap(),
            &[SqlValue::Integer(partitions[0])],
            &[SqlValue::Integer(partitions[1])],
        )
        .await
        .unwrap();
    assert_eq!(frames.len(), 4);
    assert!(
        b.snapshot(f.scope("attachments", partitions[0]))
            .await
            .unwrap()
            .rows
            .is_empty()
    );
    assert_eq!(
        b.snapshot(f.scope("attachments", partitions[1]))
            .await
            .unwrap()
            .rows
            .len(),
        1
    );
    let destination = b.snapshot(f.scope("items", partitions[1])).await.unwrap();
    let mut null = Vec::new();
    SqlValue::Null.encode(&mut null);
    assert_eq!(destination.rows.values().next().unwrap()[&field], null);
    let reader = f
        .open(
            &root.path().join("reader"),
            PrivateKey::new(),
            f.grants(Some(&[partitions[1]]), &[Right::Read]),
            None,
        )
        .await;
    f.keys(&reader, partitions[1], TABLES).await;
    let all = crate::tests::stored_frames(
        &b,
        "SELECT header,body FROM secure_operations ORDER BY sequence",
    )
    .await;
    for frame in all {
        if frame.scope().unwrap().encryption == f.scope("lists", partitions[1]).encryption
            || TABLES
                .iter()
                .any(|name| frame.scope().unwrap() == f.scope(name, partitions[1]))
        {
            reader.receive(frame).await.unwrap();
        }
    }
    assert_eq!(
        reader
            .snapshot(f.scope("attachments", partitions[1]))
            .await
            .unwrap()
            .rows
            .len(),
        1
    );
    for name in ["items", "attachments"] {
        let checkpoint = b.checkpoint(f.scope(name, partitions[1])).await.unwrap();
        assert_eq!(checkpoint.statement.format, 9);
        assert_eq!(checkpoint.statement.moves.count, 1);
        b.rebuild_projection(f.scope(name, partitions[1]))
            .await
            .unwrap();
        b.audit(f.scope(name, partitions[1])).await.unwrap();
    }
}

#[tokio::test]
async fn missing_child_holding_and_interrupted_move_leave_everything_unchanged() {
    let f = Fixture::new();
    let root = tempfile::tempdir().unwrap();
    let partitions = [
        RowIdentity::new(1, 1).unwrap().integer(),
        RowIdentity::new(1, 2).unwrap().integer(),
    ];
    let a = phone(&f, &root.path().join("a"), 1, &partitions).await;
    let b = phone(&f, &root.path().join("b"), 2, &partitions).await;
    setup(&f, &a, &b, &partitions).await;
    let mover = session(&f, &b, &partitions).await;
    b.inner.lock().await.fail_before_commit = 1;
    assert!(
        mover
            .move_row(
                [60; 32],
                "items",
                RowIdentity::new(1, 1).unwrap(),
                &[SqlValue::Integer(partitions[0])],
                &[SqlValue::Integer(partitions[1])]
            )
            .await
            .is_err()
    );
    assert_eq!(title(&f, &b, partitions[0]).await.as_deref(), Some("Item"));
    assert_eq!(title(&f, &b, partitions[1]).await, None);
    b.evict_key(f.scope("attachments", partitions[0])).await;
    let mover = session(&f, &b, &partitions).await;
    let error = mover
        .move_row(
            [60; 32],
            "items",
            RowIdentity::new(1, 1).unwrap(),
            &[SqlValue::Integer(partitions[0])],
            &[SqlValue::Integer(partitions[1])],
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("attachments"), "{error}");
    assert_eq!(title(&f, &b, partitions[0]).await.as_deref(), Some("Item"));
    assert_eq!(title(&f, &b, partitions[1]).await, None);
}

#[tokio::test]
async fn two_move_chain_follows_original_update_and_reopen_does_not_repeat() {
    let f = Fixture::new();
    let root = tempfile::tempdir().unwrap();
    let a_path = root.path().join("a");
    let partitions = [1, 2, 3].map(|sequence| RowIdentity::new(1, sequence).unwrap().integer());
    let a = phone(&f, &a_path, 1, &partitions).await;
    let b = phone(&f, &root.path().join("b"), 2, &partitions).await;
    let row = setup(&f, &a, &b, &partitions).await;
    edit(&f, &a, &row, "ticked", 70).await;
    let mover = session(&f, &b, &partitions).await;
    let first = mover
        .move_row(
            [71; 32],
            "items",
            RowIdentity::new(1, 1).unwrap(),
            &[SqlValue::Integer(partitions[0])],
            &[SqlValue::Integer(partitions[1])],
        )
        .await
        .unwrap();
    let second = mover
        .move_row(
            [72; 32],
            "items",
            RowIdentity::new(1, 1).unwrap(),
            &[SqlValue::Integer(partitions[1])],
            &[SqlValue::Integer(partitions[2])],
        )
        .await
        .unwrap();
    send(&first, &a).await;
    send(&second, &a).await;
    assert_eq!(
        title(&f, &a, partitions[2]).await.as_deref(),
        Some("ticked")
    );
    let (config, authors) = {
        let inner = a.inner.lock().await;
        (clone_config(&inner.config), inner.move_authors())
    };
    let before = crate::storage::rows(&a.inner.lock().await.conn, "SELECT * FROM secure_followed")
        .await
        .unwrap();
    assert_eq!(before.len(), 2);
    drop(a);
    let a = EncryptedReplica::open(&a_path, f.app(), config)
        .await
        .unwrap();
    for (key, credentials) in authors {
        a.register_row_author(key, credentials).await.unwrap();
    }
    send(&first, &a).await;
    send(&second, &a).await;
    assert_eq!(
        title(&f, &a, partitions[2]).await.as_deref(),
        Some("ticked")
    );
    assert_eq!(
        before,
        crate::storage::rows(&a.inner.lock().await.conn, "SELECT * FROM secure_followed")
            .await
            .unwrap()
    );
}

fn clone_config(c: &SecureConfig) -> SecureConfig {
    SecureConfig {
        publisher: c.publisher,
        manifest: c.manifest.clone(),
        revocations: c.revocations.clone(),
        node: c.node.clone(),
        credentials: c.credentials.clone(),
        keys: c
            .keys
            .iter()
            .map(|key| KeyMaterial::at(key.scope, key.generation, key.key))
            .collect(),
    }
}
async fn payload(
    replica: &EncryptedReplica<TypedRows>,
    frame: &EncryptedFrame,
) -> Result<crate::wire::RowPayload, crate::Error> {
    let inner = replica.inner.lock().await;
    let operation = frame.operation()?;
    let key = inner
        .config
        .keys
        .iter()
        .find(|key| key.scope == operation.header.extensions.scope)
        .ok_or_else(|| crate::Error::Rejected("missing key".into()))?;
    crate::wire::RowPayload::decode(&crate::wire::decrypt(
        &key.key,
        &encode(&operation.header.extensions)?,
        &frame.body,
    )?)
}

#[path = "row_moves_process.rs"]
mod process;

#[tokio::test]
async fn proof_replay_and_future_stamps_require_the_current_move_contract() {
    let f = Fixture::new();
    let root = tempfile::tempdir().unwrap();
    let partitions = [
        RowIdentity::new(1, 1).unwrap().integer(),
        RowIdentity::new(1, 2).unwrap().integer(),
    ];
    let a = phone(&f, &root.path().join("a"), 1, &partitions).await;
    let b = phone(&f, &root.path().join("b"), 2, &partitions).await;
    setup(&f, &a, &b, &partitions).await;
    let frames = session(&f, &b, &partitions)
        .await
        .move_row(
            [90; 32],
            "items",
            RowIdentity::new(1, 1).unwrap(),
            &[SqlValue::Integer(partitions[0])],
            &[SqlValue::Integer(partitions[1])],
        )
        .await
        .unwrap();
    let insertion = frames
        .iter()
        .find(|frame| frame.scope().unwrap() == f.scope("items", partitions[1]))
        .unwrap();
    let payload = payload(&b, insertion).await.unwrap();
    let write = payload.clone().into_writes().remove(0);
    let carried = payload.carried(&write.change.row).unwrap().clone();
    let operation = insertion.operation().unwrap();
    let ceiling = crate::replica::Stamp {
        clock: operation.header.timestamp,
        author: operation.header.public_key,
        sequence: operation.header.seq_num,
    };
    let inner = b.inner.lock().await;
    inner.check_carried(&write, &carried, &ceiling).unwrap();
    let mut future = carried.clone();
    future.stamps.values_mut().next().unwrap().clock = ceiling.clock + 1;
    assert!(inner.check_carried(&write, &future, &ceiling).is_err());
    let mut forged = serde_json::to_value(&carried.proof).unwrap();
    forged["version"] = serde_json::json!(2);
    let invalid: crate::replica::moves::CreationProof = serde_json::from_value(forged).unwrap();
    assert!(
        invalid
            .verify(
                f.publisher.public_key(),
                f.app().manifest().hash(),
                write.change.scope.table,
                RowIdentity::new(1, 1).unwrap(),
                &ceiling
            )
            .is_err()
    );
    assert!(
        carried
            .proof
            .verify(
                f.publisher.public_key(),
                f.app().manifest().hash(),
                write.change.scope.table,
                RowIdentity::new(1, 2).unwrap(),
                &ceiling
            )
            .is_err()
    );
    drop(inner);
    // A kept foreign identity does not become an ordinary insert permission.
    let mut ordinary = f.write("items", partitions[0], 1, None);
    ordinary.change.scope = f.scope("items", partitions[1]);
    assert!(
        b.author_bundle([91; 32], vec![ordinary], vec![])
            .await
            .is_err()
    );
    assert_eq!(
        b.receive(insertion.clone()).await.unwrap(),
        ReceiveStatus::Duplicate
    );
}

#[tokio::test]
async fn source_finalization_and_destination_horizon_fence_late_follows() {
    for source_cut in [false, true] {
        let f = Fixture::new();
        let root = tempfile::tempdir().unwrap();
        let partitions = [
            RowIdentity::new(1, 1).unwrap().integer(),
            RowIdentity::new(1, 2).unwrap().integer(),
        ];
        let a = phone(&f, &root.path().join("a"), 1, &partitions).await;
        let b = phone(&f, &root.path().join("b"), 2, &partitions).await;
        let row = setup(&f, &a, &b, &partitions).await;
        edit(&f, &a, &row, "ticked", 100).await;
        let moved = session(&f, &b, &partitions)
            .await
            .move_row(
                [101; 32],
                "items",
                RowIdentity::new(1, 1).unwrap(),
                &[SqlValue::Integer(partitions[0])],
                &[SqlValue::Integer(partitions[1])],
            )
            .await
            .unwrap();
        let cut_scope = f.scope("items", partitions[usize::from(!source_cut)]);
        let cut = rocky_credentials::SignedHorizon::sign(
            b.horizon_statement(cut_scope, &[]).await.unwrap(),
            &f.publisher,
        )
        .unwrap();
        b.receive_horizon(cut).await.unwrap();
        b.finalize_horizon(cut_scope).await.unwrap();
        if source_cut {
            super::horizon::restore_scope(&b, &a, cut_scope).await;
        }
        send(&moved, &a).await;
        if !source_cut {
            let frames = crate::tests::stored_frames(&a, "SELECT header,body FROM secure_operations WHERE status='applied' ORDER BY sequence").await;
            for frame in frames {
                b.receive(frame).await.unwrap();
            }
            assert_eq!(title(&f, &b, partitions[1]).await.as_deref(), Some("Item"));
            super::horizon::restore_scope(&b, &a, cut_scope).await;
        }
        assert_eq!(title(&f, &a, partitions[1]).await.as_deref(), Some("Item"));
        assert_eq!(title(&f, &b, partitions[1]).await.as_deref(), Some("Item"));
    }
}

#[tokio::test]
async fn each_commit_follows_marked_moves_without_scanning_stored_rows() {
    let f = Fixture::new();
    let root = tempfile::tempdir().unwrap();
    let partitions = [RowIdentity::new(1, 1).unwrap().integer()];
    let a = phone(&f, &root.path().join("a"), 1, &partitions).await;
    let b = phone(&f, &root.path().join("b"), 2, &partitions).await;
    setup(&f, &a, &b, &partitions).await;
    let inner = b.inner.lock().await;
    let plan = crate::storage::collect_rows(
        inner
            .conn
            .query(
                &format!("EXPLAIN QUERY PLAN {}", crate::replica::moves::MOVE_SCAN),
                (),
            )
            .await
            .unwrap(),
    )
    .await
    .unwrap();
    let details: Vec<_> = plan
        .iter()
        .filter_map(|row| match row.last() {
            Some(SqlValue::Text(detail)) => Some(detail.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(details.first(), Some(&"SCAN m"), "{details:?}");
    assert!(
        details.iter().all(|detail| !detail.starts_with("SCAN r")),
        "{details:?}"
    );
}
