use super::*;

#[tokio::test]
async fn warmed_dictionaries_refuse_corruption_without_repairing_authoritative_state() {
    let fixture = Typed::new(UpdatePolicy::Changed);
    let directory = TempDir::new().unwrap();
    let node = PrivateKey::new();
    let replica = fixture
        .open(
            &directory.path().join("dictionary.db"),
            &node,
            fixture.full(),
            &[fixture.items],
        )
        .await;
    insert(
        &sql(&replica, &[fixture.items]).await,
        "INSERT INTO items(key,title,done) VALUES(?,?,?)",
        vec![text("key"), text("title"), SqlValue::Integer(0)],
    )
    .await
    .unwrap();
    replica.audit(fixture.items).await.unwrap();
    let names = crate::storage::rows(
        &replica
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .conn,
        "SELECT id,name FROM secure_field_names ORDER BY id LIMIT 1",
    )
    .await
    .unwrap();
    let [SqlValue::Integer(id), SqlValue::Text(name)] = names[0].as_slice() else {
        panic!("field name");
    };
    replica
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .conn
        .execute(
            "UPDATE secure_field_names SET name=? WHERE id=?",
            crate::storage::bindings(vec![
                SqlValue::Text(format!("{name}_corrupt")),
                SqlValue::Integer(*id),
            ]),
        )
        .await
        .unwrap();
    let before = crate::tests::compaction::audit::snapshot(&replica).await;
    assert!(replica.audit(fixture.items).await.is_err());
    assert_eq!(
        crate::tests::compaction::audit::snapshot(&replica).await,
        before
    );
    replica
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .conn
        .execute(
            "UPDATE secure_field_names SET name=? WHERE id=?",
            crate::storage::bindings(vec![SqlValue::Text(name.clone()), SqlValue::Integer(*id)]),
        )
        .await
        .unwrap();
    replica.audit(fixture.items).await.unwrap();
    let origins = crate::storage::rows(
        &replica
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .conn,
        "SELECT id,origin FROM secure_row_origins",
    )
    .await
    .unwrap();
    assert_eq!(origins.len(), 1);
    let [SqlValue::Integer(id), SqlValue::Blob(origin)] = origins[0].as_slice() else {
        panic!("origin");
    };
    replica
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .conn
        .execute(
            "UPDATE secure_row_origins SET origin=X'FF' WHERE id=?",
            [*id],
        )
        .await
        .unwrap();
    let before = crate::tests::compaction::audit::snapshot(&replica).await;
    assert!(replica.audit(fixture.items).await.is_err());
    assert_eq!(
        crate::tests::compaction::audit::snapshot(&replica).await,
        before
    );
    replica
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .conn
        .execute(
            "UPDATE secure_row_origins SET origin=? WHERE id=?",
            crate::storage::bindings(vec![SqlValue::Blob(origin.clone()), SqlValue::Integer(*id)]),
        )
        .await
        .unwrap();
    replica.audit(fixture.items).await.unwrap();
    let sets = crate::storage::rows(
        &replica
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .conn,
        "SELECT id,authors FROM secure_row_authors",
    )
    .await
    .unwrap();
    assert_eq!(sets.len(), 1);
    let [SqlValue::Integer(id), SqlValue::Blob(original)] = sets[0].as_slice() else {
        panic!("author set");
    };
    let mut authors: std::collections::BTreeMap<u32, PublicKey> = decode(original).unwrap();
    let replacement = PrivateKey::new().public_key();
    authors
        .values_mut()
        .for_each(|signer| *signer = replacement);
    replica
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .conn
        .execute(
            "UPDATE secure_row_authors SET authors=? WHERE id=?",
            crate::storage::bindings(vec![
                SqlValue::Blob(encode(&authors).unwrap()),
                SqlValue::Integer(*id),
            ]),
        )
        .await
        .unwrap();
    let before = crate::tests::compaction::audit::snapshot(&replica).await;
    assert!(replica.audit(fixture.items).await.is_err());
    assert_eq!(
        crate::tests::compaction::audit::snapshot(&replica).await,
        before
    );
    replica
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .conn
        .execute(
            "UPDATE secure_row_authors SET authors=? WHERE id=?",
            crate::storage::bindings(vec![
                SqlValue::Blob(original.clone()),
                SqlValue::Integer(*id),
            ]),
        )
        .await
        .unwrap();
    replica.audit(fixture.items).await.unwrap();
    let creations = crate::storage::rows(
        &replica
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .conn,
        "SELECT id,hash FROM secure_row_creations",
    )
    .await
    .unwrap();
    assert_eq!(creations.len(), 1);
    let [SqlValue::Integer(id), SqlValue::Blob(original)] = creations[0].as_slice() else {
        panic!("creation frame reference");
    };
    replica
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .conn
        .execute(
            "UPDATE secure_row_creations SET hash=? WHERE id=?",
            crate::storage::bindings(vec![SqlValue::Blob(vec![0; 32]), SqlValue::Integer(*id)]),
        )
        .await
        .unwrap();
    let before = crate::tests::compaction::audit::snapshot(&replica).await;
    assert!(replica.audit(fixture.items).await.is_err());
    assert_eq!(
        crate::tests::compaction::audit::snapshot(&replica).await,
        before
    );
    replica
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .conn
        .execute(
            "UPDATE secure_row_creations SET hash=? WHERE id=?",
            crate::storage::bindings(vec![
                SqlValue::Blob(original.clone()),
                SqlValue::Integer(*id),
            ]),
        )
        .await
        .unwrap();
    replica.audit(fixture.items).await.unwrap();
    for (table, column) in [
        ("secure_creation_headers", "header"),
        ("secure_author_bindings", "bindings"),
    ] {
        let witnesses = crate::storage::rows(
            &replica
                .inner
                .lock(crate::replica::Class::Background)
                .await
                .conn,
            &format!("SELECT digest,{column} FROM {table} LIMIT 1"),
        )
        .await
        .unwrap();
        let [SqlValue::Blob(digest), SqlValue::Blob(original)] = witnesses[0].as_slice() else {
            panic!("stored witness");
        };
        replica
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .conn
            .execute(
                format!("DELETE FROM {table} WHERE digest=?"),
                [digest.clone()],
            )
            .await
            .unwrap();
        let before = crate::tests::compaction::audit::snapshot(&replica).await;
        assert!(replica.audit(fixture.items).await.is_err());
        assert_eq!(
            crate::tests::compaction::audit::snapshot(&replica).await,
            before
        );
        replica
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .conn
            .execute(
                format!("INSERT INTO {table} VALUES(?,?)"),
                [digest.clone(), original.clone()],
            )
            .await
            .unwrap();
        replica.audit(fixture.items).await.unwrap();
        let mut altered = original.clone();
        let last = altered.last_mut().unwrap();
        *last ^= 1;
        let statement = format!("UPDATE {table} SET {column}=? WHERE digest=?");
        replica
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .conn
            .execute(&statement, [altered, digest.clone()])
            .await
            .unwrap();
        let before = crate::tests::compaction::audit::snapshot(&replica).await;
        assert!(replica.audit(fixture.items).await.is_err());
        assert_eq!(
            crate::tests::compaction::audit::snapshot(&replica).await,
            before
        );
        replica
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .conn
            .execute(&statement, [original.clone(), digest.clone()])
            .await
            .unwrap();
        replica.audit(fixture.items).await.unwrap();
    }
}
