use super::*;
mod dictionaries;
mod streams;

#[tokio::test]
async fn seven_field_fifty_byte_rows_meet_wire_and_storage_gates_and_keep_birth_commitments() {
    let base = Typed::new(UpdatePolicy::Changed);
    let mut schema = base.schema.clone();
    let items = schema
        .tables
        .iter_mut()
        .find(|table| table.sql_name == "items")
        .unwrap();
    items.columns.extend([
        column("a", Affinity::Text, false),
        column("b", Affinity::Text, false),
    ]);
    items.create_sql = format!("{},a TEXT,b TEXT)", items.create_sql.trim_end_matches(')'));
    let fixture = Typed::with_schema(base.publisher, schema);
    let directory = TempDir::new().unwrap();
    let node = PrivateKey::from_bytes(&[194; 32]);
    let path = directory.path().join("compact.db");
    let replica = fixture
        .open(&path, &node, fixture.full(), &[fixture.items])
        .await;
    let writer = sql(&replica, &[fixture.items]).await;
    writer.run("BEGIN", vec![]).await.unwrap();
    for index in 0..32 {
        let id = writer.next_id("items").await.unwrap();
        writer
            .run(
                "INSERT INTO items(key,title,done,id,a,b) VALUES(?,'t',0,?,'','')",
                vec![
                    text(&format!("{index:02}")),
                    SqlValue::Integer(id.integer()),
                ],
            )
            .await
            .unwrap();
    }
    let frames = writer
        .commit([94; 32])
        .await
        .unwrap()
        .committed
        .unwrap()
        .frames;
    assert_eq!(frames.len(), 1);
    let frame = &frames[0];
    let operation = frame.operation_ref().unwrap();
    let plaintext = crate::wire::decrypt(
        &fixture.key(fixture.items).key,
        &encode(&operation.header.extensions).unwrap(),
        frame.body_bytes(),
    )
    .unwrap();
    let inner = replica.inner.lock().await;
    let payload = inner.decode_row_payload(frame, &plaintext).unwrap();
    assert_eq!(payload.writes().len(), 32);
    let bindings: Vec<_> = frame
        .chain()
        .unwrap()
        .credentials()
        .0
        .iter()
        .map(|link| link.author_binding.clone())
        .collect();
    let bindings = rocky_core::commitment("rocky.author-bindings.v1", &encode(&bindings).unwrap());
    for write in payload.writes() {
        let fields = write.change.after.as_ref().unwrap();
        assert_eq!(fields.len(), 7);
        assert_eq!(fields.values().map(Vec::len).sum::<usize>(), 50);
        let identity = fixture.app().row_identity(&write.change).unwrap().unwrap();
        let statement = rocky_core::CreationStatement::derived(
            fixture.app().manifest().hash(),
            fixture.items.table,
            identity,
            crate::replica::Stamp {
                clock: operation.header.timestamp,
                author: operation.header.extensions.author.unwrap(),
                sequence: operation.header.extensions.author_sequence,
            },
            bindings,
        );
        let signed = statement.canonical_bytes();
        assert!(node.public_key().verify(&signed, &node.sign(&signed)));
        assert_eq!(
            payload
                .proof(&write.change.row)
                .unwrap()
                .provenance()
                .unwrap(),
            rocky_core::commitment("rocky.creation-provenance.v1", &signed)
        );
        let mut altered = statement;
        altered.bindings[0] ^= 1;
        assert!(
            !operation
                .header
                .extensions
                .births
                .iter()
                .any(|birth| birth.as_ref() == &altered.commitment())
        );
    }
    let stamp = crate::replica::Stamp {
        clock: operation.header.timestamp,
        author: operation.header.extensions.author.unwrap(),
        sequence: operation.header.extensions.author_sequence,
    };
    let carries: std::collections::BTreeMap<_, _> = payload
        .writes()
        .iter()
        .map(|write| {
            (
                write.change.row,
                crate::replica::moves::Carried {
                    proof: payload.proof(&write.change.row).unwrap().clone(),
                    stamps: write
                        .change
                        .after
                        .as_ref()
                        .unwrap()
                        .keys()
                        .map(|field| (*field, stamp.clone()))
                        .collect(),
                    authors: [(stamp.author, operation.header.public_key)].into(),
                    moved_from: None,
                },
            )
        })
        .collect();
    let carried = crate::wire::RowPayload::compact(
        &crate::wire::RowPayload::witness(
            crate::wire::RowPayload::stamp_body(
                &crate::wire::RowPayload::rows(
                    &payload
                        .writes()
                        .iter()
                        .map(|write| write.change.clone())
                        .collect::<Vec<_>>(),
                )
                .unwrap(),
                Default::default(),
                carries.clone(),
            )
            .unwrap(),
            Vec::new(),
        )
        .unwrap(),
        &fixture.app().manifest(),
        fixture.items,
    )
    .unwrap();
    assert!(
        carried.len() + 40 <= 16_000,
        "carried body {} bytes",
        carried.len()
    );
    let expanded =
        crate::wire::RowPayload::decode_source(&carried, &fixture.app().manifest(), fixture.items)
            .unwrap();
    for (row, original) in carries {
        let restored = expanded.carried(&row).unwrap();
        assert_eq!(
            encode(&restored.proof).unwrap(),
            encode(&original.proof).unwrap()
        );
        assert_eq!(restored.stamps, original.stamps);
        assert_eq!(restored.authors, original.authors);
    }
    eprintln!("COMPACT_CARRY rows=32 plaintext_body={}", carried.len());
    let records = collect_rows(
        inner
            .conn
            .query("SELECT fields FROM secure_row_records", ())
            .await
            .unwrap(),
    )
    .await
    .unwrap();
    let mut maximum = 0;
    for row in records {
        let [SqlValue::Blob(bytes)] = row.as_slice() else {
            panic!("stored row");
        };
        maximum = maximum.max(bytes.len());
        assert!(bytes.len() <= 300, "stored row is {} bytes", bytes.len());
    }
    assert!(
        frame.stored_bytes().unwrap() <= 32 * 500,
        "frame is {} bytes",
        frame.stored_bytes().unwrap()
    );
    assert!(frame.body_bytes().len() <= 16 * 1024);
    eprintln!(
        "COMPACT rows=32 application_bytes_per_row=50 frame_body={} frame_stored={} record_max={maximum}",
        frame.body_bytes().len(),
        frame.stored_bytes().unwrap()
    );
    drop(inner);
    replica.audit(fixture.items).await.unwrap();
    let mut body = frame.body_bytes().to_vec();
    body[0] ^= 1;
    assert!(
        EncryptedFrame::bare(frame.header_bytes().to_vec(), body)
            .operation_ref()
            .is_err()
    );
    drop(writer);
    drop(replica);
    let reopened = fixture
        .open(&path, &node, fixture.full(), &[fixture.items])
        .await;
    assert_eq!(
        select(
            &sql(&reopened, &[fixture.items]).await,
            "SELECT count(*) FROM items WHERE title='t' AND a='' AND b=''"
        )
        .await,
        vec![vec![SqlValue::Integer(32)]]
    );
    reopened.audit(fixture.items).await.unwrap();
}

#[tokio::test]
async fn a_full_sixty_four_frame_bundle_keeps_membership_within_the_frame_budget() {
    let fixture = Typed::new(UpdatePolicy::Changed);
    let directory = TempDir::new().unwrap();
    let replica = fixture
        .open(
            &directory.path().join("full-bundle.db"),
            &PrivateKey::new(),
            fixture.full(),
            &[fixture.items],
        )
        .await;
    let writer = sql(&replica, &[fixture.items]).await;
    writer.run("BEGIN", vec![]).await.unwrap();
    for index in 0..crate::MAX_BUNDLE_ROWS {
        let id = writer.next_id("items").await.unwrap();
        writer
            .run(
                "INSERT INTO items(key,title,done,id) VALUES(?,'title',0,?)",
                vec![text(&index.to_string()), SqlValue::Integer(id.integer())],
            )
            .await
            .unwrap();
    }
    let frames = writer
        .commit([95; 32])
        .await
        .unwrap()
        .committed
        .unwrap()
        .frames;
    assert_eq!(frames.len(), 64);
    let total: usize = frames.iter().map(|frame| frame.body_bytes().len()).sum();
    assert!(
        total <= frames.len() * 16_000,
        "average body {}",
        total / frames.len()
    );
    for frame in &frames {
        let operation = frame.operation_ref().unwrap();
        let plaintext = crate::wire::decrypt(
            &fixture.key(fixture.items).key,
            &encode(&operation.header.extensions).unwrap(),
            frame.body_bytes(),
        )
        .unwrap();
        let inner = replica.inner.lock().await;
        let payload = inner.decode_row_payload(frame, &plaintext).unwrap();
        assert_eq!(payload.membership().unwrap().members.len(), 64);
        assert_eq!(payload.writes().len(), 32);
    }
    replica.audit(fixture.items).await.unwrap();
}
