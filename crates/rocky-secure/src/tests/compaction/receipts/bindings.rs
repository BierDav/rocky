use super::*;

async fn two_births(replica: &Replica) -> EncryptedFrame {
    let sql = session(replica).await;
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
    let frames = stored_frames(
        replica,
        "SELECT header,body FROM secure_operations ORDER BY sequence DESC LIMIT 1",
    )
    .await;
    assert_eq!(frames.len(), 1);
    frames[0].clone()
}
fn start_trace() {
    crate::BINDING_TRACE.with(|trace| *trace.borrow_mut() = Some(Vec::new()));
}
fn trace() -> Vec<(usize, usize, usize)> {
    crate::BINDING_TRACE.with(|trace| trace.borrow_mut().take().unwrap())
}
async fn follower(
    lineage: &Lineage,
    dir: &TempDir,
    source: &Replica,
    name: &str,
    key: &PrivateKey,
) -> Replica {
    let replica = open(lineage, dir, name, key).await;
    replica
        .receive_tip_statements(&crate::tests::all_tip_statements(source).await)
        .await
        .unwrap();
    replica
        .follow_bookmark(lineage.domain, "main")
        .await
        .unwrap();
    replica
}

#[tokio::test]
async fn ordinary_births_share_only_one_frame_local_historical_binding_success() {
    let lineage = Lineage::pinned(231);
    let dir = TempDir::new().unwrap();
    let key = PrivateKey::from_bytes(&[232; 32]);
    let reader_key = PrivateKey::from_bytes(&[233; 32]);
    let source = open(&lineage, &dir, "source", &key).await;
    follow_main(&lineage, &source).await;
    let frame = two_births(&source).await;
    let reader = follower(&lineage, &dir, &source, "reader", &reader_key).await;
    start_trace();
    assert_eq!(
        reader.receive(frame.clone()).await.unwrap(),
        ReceiveStatus::Applied
    );
    assert_eq!(trace(), vec![(2, 1, 1)]);
    assert_eq!(
        reader
            .snapshot(lineage.scope("items"))
            .await
            .unwrap()
            .rows
            .len(),
        2
    );
    assert_eq!(
        scalar(&reader, "SELECT count(*) FROM secure_applied").await,
        1
    );
    assert_eq!(
        scalar(&reader, "SELECT count(*) FROM secure_admission_entries").await,
        1
    );
    assert_eq!(
        records(
            &reader,
            "SELECT ordinal FROM secure_admission_claims ORDER BY ordinal"
        )
        .await,
        vec![vec![SqlValue::Integer(0)], vec![SqlValue::Integer(1)]]
    );
    for table in [
        "secure_identity_marks",
        "secure_identity_own",
        "secure_identity_ledger",
    ] {
        assert_eq!(
            scalar(&reader, &format!("SELECT max(sequence) FROM {table}")).await,
            2
        );
    }
    assert_eq!(
        titles(&reader).await,
        vec![
            vec![text("one"), text("one")],
            vec![text("two"), text("two")]
        ]
    );
    insert(&source, "three", "three").await;
    let next = stored_frames(
        &source,
        "SELECT header,body FROM secure_operations ORDER BY sequence DESC LIMIT 1",
    )
    .await;
    start_trace();
    assert_eq!(
        reader.receive(next[0].clone()).await.unwrap(),
        ReceiveStatus::Applied
    );
    assert_eq!(trace(), vec![(1, 1, 0)]);
    reader.audit(lineage.scope("items")).await.unwrap();
    drop(reader);
    let reopened = lineage
        .reopen(&dir.path().join("reader.db"), &reader_key, 3, 3)
        .await
        .unwrap();
    reopened.audit(lineage.scope("items")).await.unwrap();
    assert_eq!(
        scalar(&reopened, "SELECT count(*) FROM secure_admission_claims").await,
        3
    );
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Envelope {
    version: u8,
    #[serde(with = "serde_bytes")]
    body: Vec<u8>,
    creations: Vec<(Commitment, Commitment)>,
}
async fn damaged_frame(
    source: &Replica,
    frame: &EncryptedFrame,
    node: &PrivateKey,
    lower: bool,
) -> EncryptedFrame {
    let operation = frame.operation_ref().unwrap();
    let key = source
        .inner
        .lock()
        .await
        .config
        .keys
        .iter()
        .find(|key| key.scope == operation.header.extensions.scope)
        .unwrap()
        .clone();
    let plaintext = crate::wire::decrypt(
        &key.key,
        &encode(&operation.header.extensions).unwrap(),
        frame.body_bytes(),
    )
    .unwrap();
    let envelope: Envelope = crate::decode(&plaintext).unwrap();
    assert_eq!(envelope.version, 1);
    let payload = source
        .inner
        .lock()
        .await
        .decode_row_payload(frame, &plaintext)
        .unwrap();
    assert_eq!(payload.writes().len(), 2);
    let mut extension = operation.header.extensions.clone();
    let target = &payload.writes()[1];
    let mut proof = payload.proof(&target.change.row).unwrap().clone();
    let original = proof.provenance().unwrap();
    let index = extension
        .births
        .iter()
        .position(|birth| birth.as_ref() == &original)
        .unwrap();
    if lower {
        proof.lower_stamp_for_test(node);
        extension.births[index] = proof.provenance().unwrap().into();
    } else {
        // An ordinary birth cannot carry a second, damaged signature; tamper
        // its signed statement commitment instead.
        extension.births[index] = [99; 32].into();
    }
    crate::wire::author(
        node,
        extension,
        frame.chain().unwrap().clone(),
        &key.key,
        (
            operation.header.seq_num,
            operation.header.backlink,
            operation.header.timestamp,
        ),
        operation.header.previous.clone(),
        &plaintext,
    )
    .unwrap()
}
async fn refused_second_proof(lower: bool) {
    let lineage = Lineage::pinned(234);
    let dir = TempDir::new().unwrap();
    let key = PrivateKey::from_bytes(&[235; 32]);
    let source = open(&lineage, &dir, "source", &key).await;
    follow_main(&lineage, &source).await;
    let frame = two_births(&source).await;
    let damaged = damaged_frame(&source, &frame, &key, lower).await;
    let reader = follower(
        &lineage,
        &dir,
        &source,
        "reader",
        &PrivateKey::from_bytes(&[236; 32]),
    )
    .await;
    reader
        .stage_bundle_before_apply_for_test(std::slice::from_ref(&damaged))
        .await
        .unwrap();
    let before = crate::tests::bundles::payload::state(&reader).await;
    start_trace();
    let reason = reader
        .apply_ready_bundle_for_test(std::slice::from_ref(&damaged))
        .await
        .unwrap();
    assert_eq!(
        reason,
        "replication data rejected: frame creation statements differ from its births"
    );
    assert!(trace().is_empty());
    assert_eq!(crate::tests::bundles::payload::state(&reader).await, before);
    assert_eq!(
        reader.receipt_status(damaged.id().unwrap()).await.unwrap(),
        ReceiveStatus::Rejected
    );
    assert!(titles(&reader).await.is_empty());
}
#[tokio::test]
async fn a_warm_binding_never_skips_a_later_own_proof_signature() {
    refused_second_proof(false).await;
}
#[tokio::test]
async fn a_valid_older_proof_still_fails_the_exact_creating_frame_stamp() {
    refused_second_proof(true).await;
}
#[tokio::test]
async fn binding_memo_requests_and_per_proof_errors_match_the_uncached_verifier() {
    let lineage = Lineage::pinned(227);
    let dir = TempDir::new().unwrap();
    let node = PrivateKey::from_bytes(&[228; 32]);
    let source = open(&lineage, &dir, "source", &node).await;
    follow_main(&lineage, &source).await;
    let frame = two_births(&source).await;
    let config = lineage.config(3, &node);
    let operation = frame.operation_ref().unwrap();
    let key = config
        .keys
        .iter()
        .find(|key| key.scope == operation.header.extensions.scope)
        .unwrap();
    let payload = source
        .inner
        .lock()
        .await
        .decode_row_payload(
            &frame,
            &crate::wire::decrypt(
                &key.key,
                &encode(&operation.header.extensions).unwrap(),
                frame.body_bytes(),
            )
            .unwrap(),
        )
        .unwrap();
    payload
        .proof(&payload.writes()[0].change.row)
        .unwrap()
        .check_binding_differentials_for_test(&node, config.publisher);
}
