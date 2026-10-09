use super::*;

async fn peer(replica: &EncryptedReplica<TypedRows>) -> (PublicKey, CredentialChain) {
    let inner = replica.inner.lock().await;
    (
        inner.config.node.public_key(),
        inner.config.credentials.clone(),
    )
}
async fn restore_scope(
    from: &EncryptedReplica<TypedRows>,
    to: &EncryptedReplica<TypedRows>,
    scope: ResourceScope,
) {
    to.subscribe(scope).await.unwrap();
    let checkpoint = from.checkpoint(scope).await.unwrap();
    let (node, credentials) = peer(to).await;
    to.begin_rebootstrap(checkpoint.clone()).await.unwrap();
    let mut after = None;
    loop {
        let page = from
            .checkpoint_page(
                node,
                &credentials,
                scope,
                checkpoint.id().unwrap(),
                after,
                65536,
            )
            .await
            .unwrap();
        after = page.next;
        to.stage_checkpoint_page(page).await.unwrap();
        if after.is_none() {
            break;
        }
    }
    to.finish_rebootstrap(scope).await.unwrap();
}
async fn partition_state(
    f: &Fixture,
    replica: &EncryptedReplica<TypedRows>,
    partition: i64,
) -> Vec<Vec<Vec<SqlValue>>> {
    let inner = replica.inner.lock().await;
    let mut state = Vec::new();
    for table in ["lists", "categories", "items"] {
        let scope = f.scope(table, partition);
        for sql in [
            "SELECT * FROM secure_rows WHERE scope=? ORDER BY row".to_string(),
            "SELECT * FROM secure_support WHERE scope=? ORDER BY tab,id".to_string(),
            "SELECT * FROM secure_row_keys WHERE scope=? ORDER BY row".to_string(),
            "SELECT * FROM secure_identity_marks WHERE scope=? ORDER BY author".to_string(),
            "SELECT * FROM secure_scope_state WHERE scope=?".to_string(),
            "SELECT * FROM secure_horizon_pending WHERE scope=? ORDER BY dependency".to_string(),
            "SELECT * FROM secure_checkpoints WHERE scope=(SELECT id FROM secure_scope_state WHERE scope=?) ORDER BY sequence".to_string(),
            "SELECT * FROM secure_checkpoint_rows WHERE scope=(SELECT id FROM secure_scope_state WHERE scope=?) ORDER BY row".to_string(),
            "SELECT * FROM secure_checkpoint_stubs WHERE scope=(SELECT id FROM secure_scope_state WHERE scope=?) ORDER BY hash".to_string(),
            "SELECT * FROM secure_stubs WHERE scope=(SELECT id FROM secure_scope_state WHERE scope=?) ORDER BY hash".to_string(),
        ] {
            state.push(
                crate::storage::collect_rows(
                    inner
                        .conn
                        .query(sql, [encode(&scope).unwrap()])
                        .await
                        .unwrap(),
                )
                .await
                .unwrap(),
            );
        }
        let column = if table == "lists" { "id" } else { "list" };
        for name in [table.to_string(), format!("secure_candidates_{table}")] {
            state.push(
                crate::storage::collect_rows(
                    inner
                        .conn
                        .query(
                            format!("SELECT * FROM {name} WHERE {column}=? ORDER BY id"),
                            [partition],
                        )
                        .await
                        .unwrap(),
                )
                .await
                .unwrap(),
            );
        }
    }
    state
}
async fn declare(
    f: &Fixture,
    replica: &EncryptedReplica<TypedRows>,
    scope: ResourceScope,
    peers: &[(PublicKey, CredentialChain)],
) -> rocky_credentials::SignedHorizon {
    let signed = rocky_credentials::SignedHorizon::sign(
        replica.horizon_statement(scope, peers).await.unwrap(),
        &f.publisher,
    )
    .unwrap();
    replica.receive_horizon(signed.clone()).await.unwrap();
    signed
}
fn delete(write: &RowWrite) -> RowWrite {
    RowWrite::changed(RowChange {
        scope: write.change.scope,
        row: write.change.row,
        before: write.change.after.clone(),
        after: None,
    })
}

#[tokio::test]
async fn group_horizon_finalizes_losers_orphans_and_recovers_each_atomic_step() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::new();
    let a = RowIdentity::new(1, 1).unwrap().integer();
    let b = RowIdentity::new(1, 2).unwrap().integer();
    let writer = f
        .open(
            &root.path().join("writer"),
            PrivateKey::new(),
            f.grants(
                None,
                &[Right::Read, Right::Insert, Right::Update, Right::Delete],
            ),
            Some((PrivateKey::new(), 1)),
        )
        .await;
    let reader_node = PrivateKey::new();
    let reader_path = root.path().join("reader");
    let mut reader = f
        .open(
            &reader_path,
            reader_node.clone(),
            f.grants(None, &[Right::Read]),
            None,
        )
        .await;
    for device in [&writer, &reader] {
        for partition in [a, b] {
            f.keys(device, partition, &["lists", "categories", "items"])
                .await;
        }
    }
    let offline = f
        .open(
            &root.path().join("offline"),
            PrivateKey::new(),
            f.grants(Some(&[a]), &[Right::Read]),
            None,
        )
        .await;
    f.keys(&offline, a, &["lists", "categories", "items"]).await;
    let list = f.write("lists", a, 1, None);
    let cat = f.write("categories", a, 1, None);
    let category = RowIdentity::new(1, 1).unwrap().integer();
    let writes = vec![
        list.clone(),
        f.write("lists", b, 2, None),
        cat.clone(),
        f.write("items", a, 1, Some(category)),
        f.write("items", a, 2, Some(category)),
        f.write("items", b, 1, None),
        f.write("items", b, 2, None),
    ];
    for (index, write) in writes.into_iter().enumerate() {
        for frame in writer
            .author_bundle([index as u8; 32], vec![write], vec![])
            .await
            .unwrap()
        {
            if ["lists", "categories", "items"]
                .iter()
                .any(|name| frame.operation().unwrap().header.extensions.scope == f.scope(name, a))
            {
                offline.receive(frame.clone()).await.unwrap();
            }
            reader.receive(frame).await.unwrap();
        }
    }
    assert_eq!(visible(&reader, "items").await.len(), 2);
    let other = partition_state(&f, &reader, b).await;
    // Eviction and checkpoint import rebuild the whole held group partition.
    let (full_node, full_credentials) = peer(&writer).await;
    for name in ["lists", "categories", "items"] {
        reader.subscribe(f.scope(name, b)).await.unwrap();
    }
    reader
        .set_role(crate::RoleConfig {
            role: crate::ReplicaRole::Partial,
            full_nodes: [full_node].into(),
            custody: 1,
        })
        .await
        .unwrap();
    for name in ["lists", "categories", "items"] {
        let scope = f.scope(name, a);
        reader.subscribe(scope).await.unwrap();
        reader
            .acknowledge_frontier(
                full_node,
                &full_credentials,
                &writer.frontier(scope).await.unwrap(),
            )
            .await
            .unwrap();
        reader.unsubscribe(scope).await.unwrap();
        reader.evict(scope).await.unwrap();
        assert_eq!(partition_state(&f, &reader, b).await, other);
    }
    for name in ["lists", "categories", "items"] {
        restore_scope(&writer, &reader, f.scope(name, a)).await;
        assert_eq!(partition_state(&f, &reader, b).await, other);
    }
    assert_eq!(visible(&reader, "items").await.len(), 2);
    {
        let inner = reader.inner.lock().await;
        let candidates = crate::storage::collect_rows(
            inner
                .conn
                .query(
                    "SELECT COUNT(*) FROM secure_candidates_items WHERE list=?",
                    [a],
                )
                .await
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(candidates, vec![vec![SqlValue::Integer(2)]]);
    }
    let signed = declare(&f, &writer, f.scope("items", a), &[peer(&reader).await]).await;
    reader.receive_horizon(signed.clone()).await.unwrap();
    let ack = reader
        .acknowledge_horizon(f.scope("items", a))
        .await
        .unwrap();
    writer
        .receive_horizon_ack(f.scope("items", a), ack)
        .await
        .unwrap();
    reader.checkpoint(f.scope("items", a)).await.unwrap();
    for step in 1..=6 {
        let before = partition_state(&f, &reader, a).await;
        if step == 6 {
            reader.inner.lock().await.fail_before_commit = 1;
        } else {
            reader.inner.lock().await.fail_horizon_at = Some(step);
        }
        assert!(matches!(
            reader.finalize_horizon(f.scope("items", a)).await,
            Err(Error::InjectedFailure)
        ));
        assert_eq!(partition_state(&f, &reader, a).await, before);
        let config = {
            let inner = reader.inner.lock().await;
            SecureConfig {
                publisher: inner.config.publisher,
                manifest: inner.config.manifest.clone(),
                policy: inner.config.policy.clone(),
                node: inner.config.node.clone(),
                credentials: inner.config.credentials.clone(),
                keys: inner
                    .config
                    .keys
                    .iter()
                    .map(|key| KeyMaterial {
                        scope: key.scope,
                        generation: key.generation,
                        key: key.key,
                    })
                    .collect(),
            }
        };
        drop(reader);
        reader = EncryptedReplica::open(&reader_path, f.app(), config)
            .await
            .unwrap();
        assert_eq!(partition_state(&f, &reader, a).await, before);
        assert_eq!(partition_state(&f, &reader, b).await, other);
    }
    for device in [&writer, &reader] {
        let cp = device.finalize_horizon(f.scope("items", a)).await.unwrap();
        assert_eq!(cp.statement.rows.count, 1);
        assert_eq!(cp.statement.tombstones, 0);
    }
    assert_eq!(partition_state(&f, &reader, b).await, other);
    for frame in writer
        .author_bundle([20; 32], vec![delete(&cat)], vec![])
        .await
        .unwrap()
    {
        reader.receive(frame).await.unwrap();
    }
    for device in [&writer, &reader] {
        let rows = visible(device, "items").await;
        let row = rows
            .iter()
            .find(|row| row[0] == SqlValue::Integer(a))
            .unwrap();
        assert_eq!(row[1], SqlValue::Null);
    }
    for name in ["items", "categories"] {
        let scope = f.scope(name, a);
        let cut = declare(&f, &writer, scope, &[peer(&reader).await]).await;
        reader.receive_horizon(cut).await.unwrap();
        writer
            .receive_horizon_ack(scope, reader.acknowledge_horizon(scope).await.unwrap())
            .await
            .unwrap();
        for device in [&writer, &reader] {
            let cp = device.finalize_horizon(scope).await.unwrap();
            assert_eq!(cp.statement.rows.count, if name == "items" { 1 } else { 0 });
        }
    }
    for device in [&writer, &reader] {
        let rows = visible(device, "items").await;
        assert_eq!(
            rows.iter()
                .find(|row| row[0] == SqlValue::Integer(a))
                .unwrap()[1],
            SqlValue::Null
        );
    }
    for frame in writer
        .author_bundle([21; 32], vec![delete(&list)], vec![])
        .await
        .unwrap()
    {
        reader.receive(frame).await.unwrap();
    }
    for name in ["items", "categories", "lists"] {
        let scope = f.scope(name, a);
        let cut = declare(&f, &writer, scope, &[peer(&reader).await]).await;
        reader.receive_horizon(cut).await.unwrap();
        writer
            .receive_horizon_ack(scope, reader.acknowledge_horizon(scope).await.unwrap())
            .await
            .unwrap();
        for device in [&writer, &reader] {
            assert_eq!(
                device
                    .finalize_horizon(scope)
                    .await
                    .unwrap()
                    .statement
                    .rows
                    .count,
                0
            );
        }
    }
    assert_eq!(visible(&offline, "items").await.len(), 1);
    for name in ["lists", "categories", "items"] {
        restore_scope(&writer, &offline, f.scope(name, a)).await;
    }
    assert!(visible(&offline, "items").await.is_empty());
    assert_eq!(visible(&reader, "items").await.len(), 1);
    assert_eq!(partition_state(&f, &reader, b).await, other);
}

#[tokio::test]
async fn group_horizon_b_does_not_wait_for_a_and_refuses_missing_parent_rights() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::new();
    let a = RowIdentity::new(1, 1).unwrap().integer();
    let b = RowIdentity::new(1, 2).unwrap().integer();
    let writer = f
        .open(
            &root.path().join("writer"),
            PrivateKey::new(),
            f.grants(
                None,
                &[Right::Read, Right::Insert, Right::Update, Right::Delete],
            ),
            Some((PrivateKey::new(), 1)),
        )
        .await;
    let reader = f
        .open(
            &root.path().join("reader"),
            PrivateKey::new(),
            f.grants(Some(&[b]), &[Right::Read]),
            None,
        )
        .await;
    let waiting = f
        .open(
            &root.path().join("waiting"),
            PrivateKey::new(),
            f.grants(Some(&[a]), &[Right::Read]),
            None,
        )
        .await;
    for partition in [a, b] {
        f.keys(&writer, partition, &["lists", "categories", "items"])
            .await;
    }
    f.keys(&reader, b, &["lists", "categories", "items"]).await;
    declare(&f, &writer, f.scope("items", a), &[peer(&waiting).await]).await;
    let cut = declare(&f, &writer, f.scope("items", b), &[peer(&reader).await]).await;
    reader.receive_horizon(cut).await.unwrap();
    writer
        .receive_horizon_ack(
            f.scope("items", b),
            reader
                .acknowledge_horizon(f.scope("items", b))
                .await
                .unwrap(),
        )
        .await
        .unwrap();
    writer.finalize_horizon(f.scope("items", b)).await.unwrap();
    reader.finalize_horizon(f.scope("items", b)).await.unwrap();
    assert!(writer.finalize_horizon(f.scope("items", a)).await.is_err());
    assert_eq!(
        writer
            .horizon_progress(f.scope("items", a))
            .await
            .unwrap()
            .waiting
            .len(),
        1
    );
    assert!(
        writer
            .author_bundle([40; 32], vec![f.write("categories", a, 1, None)], vec![])
            .await
            .is_err()
    );
    writer
        .author_bundle([41; 32], vec![f.write("categories", b, 1, None)], vec![])
        .await
        .unwrap();
    let mut grants = f.grants(Some(&[b]), &[Right::Read]);
    grants.retain(|grant| grant.table == f.scope("items", b).table);
    let child = f
        .open(&root.path().join("child"), PrivateKey::new(), grants, None)
        .await;
    assert!(
        writer
            .horizon_statement(f.scope("items", b), &[peer(&child).await])
            .await
            .is_err()
    );
    assert!(
        writer
            .horizon_statement(f.scope("items", b), &[peer(&waiting).await])
            .await
            .is_err()
    );
}

#[tokio::test]
async fn partition_checkpoint_rebuilds_keyless_support_and_base_import_preserves_unrelated_graph() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::logged();
    let writer = f
        .open(
            &root.path().join("writer"),
            PrivateKey::new(),
            f.grants(
                None,
                &[Right::Read, Right::Insert, Right::Update, Right::Delete],
            ),
            Some((PrivateKey::new(), 1)),
        )
        .await;
    let child = f
        .open(
            &root.path().join("child"),
            PrivateKey::new(),
            f.grants(None, &[Right::Read]),
            None,
        )
        .await;
    let live = RowIdentity::new(1, 1).unwrap().integer();
    let dead = RowIdentity::new(1, 2).unwrap().integer();
    writer
        .install_key(KeyMaterial {
            scope: f.scope("products", 0),
            generation: 1,
            key: [7; 32],
        })
        .await
        .unwrap();
    for device in [&writer, &child] {
        f.keys(device, 10, &["items"]).await;
        f.keys(device, 20, &["items"]).await;
        f.keys(device, 30, &["categories"]).await;
    }
    let product = f.write("products", dead, 2, None);
    for (index, write) in [
        f.write("products", live, 1, None),
        product.clone(),
        f.write("categories", 30, 1, None),
        f.write("items", 10, 1, Some(live)),
        f.write("items", 10, 2, Some(dead)),
        f.write("items", 20, 1, Some(live)),
    ]
    .into_iter()
    .enumerate()
    {
        for frame in writer
            .author_bundle([index as u8; 32], vec![write], vec![])
            .await
            .unwrap()
        {
            if frame.operation().unwrap().header.extensions.scope != f.scope("products", 0) {
                child.receive(frame).await.unwrap();
            }
        }
    }
    writer
        .author_bundle([10; 32], vec![delete(&product)], vec![])
        .await
        .unwrap();
    sync_existence(&writer, &child, f.scope("products", 0)).await;
    assert_eq!(visible(&child, "items").await.len(), 2);
    let state = || async {
        let inner = child.inner.lock().await;
        let mut captured = Vec::new();
        for sql in [
            "SELECT * FROM secure_rows WHERE scope=? ORDER BY row",
            "SELECT * FROM secure_identity_marks WHERE scope=? ORDER BY author",
            "SELECT * FROM secure_support WHERE scope=? ORDER BY tab,id",
        ] {
            captured.push(
                crate::storage::collect_rows(
                    inner
                        .conn
                        .query(sql, [encode(&f.scope("categories", 30)).unwrap()])
                        .await
                        .unwrap(),
                )
                .await
                .unwrap(),
            );
        }
        for sql in [
            "SELECT * FROM categories ORDER BY id",
            "SELECT * FROM secure_candidates_categories ORDER BY id",
        ] {
            captured.push(
                crate::storage::collect_rows(inner.conn.query(sql, ()).await.unwrap())
                    .await
                    .unwrap(),
            );
        }
        format!("{captured:?}")
    };
    let unrelated = state().await;
    restore_scope(&writer, &child, f.scope("items", 10)).await;
    assert_eq!(state().await, unrelated);
    {
        let inner = child.inner.lock().await;
        let puppets = crate::storage::collect_rows(
            inner
                .conn
                .query("SELECT id,row_kind FROM products WHERE id=?", [live])
                .await
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(
            puppets,
            vec![vec![SqlValue::Integer(live), SqlValue::Integer(1)]]
        );
    }
    declare(&f, &writer, f.scope("products", 0), &[]).await;
    writer
        .finalize_horizon(f.scope("products", 0))
        .await
        .unwrap();
    let (node, credentials) = peer(&child).await;
    let base = writer
        .existence_base_offer(node, &credentials, f.scope("products", 0))
        .await
        .unwrap()
        .unwrap();
    let id = base.id().unwrap();
    child.begin_existence_base(base).await.unwrap();
    let page = writer
        .existence_base_page(node, &credentials, f.scope("products", 0), id, None, 256)
        .await
        .unwrap();
    child
        .stage_existence_base(f.scope("products", 0), page)
        .await
        .unwrap();
    child
        .finish_existence_base(f.scope("products", 0))
        .await
        .unwrap();
    assert_eq!(state().await, unrelated);
    let cut = declare(&f, &writer, f.scope("items", 10), &[peer(&child).await]).await;
    child.receive_horizon(cut).await.unwrap();
    writer
        .receive_horizon_ack(
            f.scope("items", 10),
            child
                .acknowledge_horizon(f.scope("items", 10))
                .await
                .unwrap(),
        )
        .await
        .unwrap();
    for device in [&writer, &child] {
        assert_eq!(
            device
                .finalize_horizon(f.scope("items", 10))
                .await
                .unwrap()
                .statement
                .rows
                .count,
            1
        );
    }
    assert_eq!(state().await, unrelated);
    assert!(child.key_epochs(f.scope("products", 0)).await.is_empty());
}
