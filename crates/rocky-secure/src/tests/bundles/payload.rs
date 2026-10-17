use super::*;
use crate::storage::rows;
use crate::wire::RowPayload;
use rocky_core::SqlValue;

#[derive(serde::Serialize, serde::Deserialize)]
struct Envelope {
    version: u8,
    #[serde(with = "serde_bytes")]
    body: Vec<u8>,
    creations: Vec<(Commitment, Commitment)>,
}

pub(in crate::tests) async fn state<A: rocky_core::RowApplication>(
    replica: &EncryptedReplica<A>,
) -> Vec<(String, Vec<Vec<SqlValue>>)> {
    let inner = replica.inner.lock().await;
    let mut snapshot = Vec::new();
    for row in rows(&inner.conn, "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name<>'secure_operations' ORDER BY name").await.unwrap() {
        let [SqlValue::Text(table)] = row.as_slice() else {
            panic!("table name");
        };
        snapshot.push((table.clone(), rows(&inner.conn, &format!("SELECT * FROM \"{table}\"" )).await.unwrap()));
    }
    snapshot.push(("operations except status".into(), rows(&inner.conn, "SELECT hash,author,scope,sequence,header,body,chain FROM secure_operations ORDER BY hash").await.unwrap()));
    snapshot
}

#[tokio::test]
async fn final_member_payloads_must_agree_on_held_resolution_before_any_materialization() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let node = PrivateKey::from_bytes(&[108; 32]);
    let both = [f.public, f.private];
    let source = open(&f, &dir.path().join("source.db"), &node, f.full(), &both).await;
    let mut frames = source
        .author_bundle(
            [1; 32],
            vec![public(&f, 1, "one"), private(&f, 1, "100")],
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(frames.len(), 2);
    let config = f.config(&node, f.full(), &both);
    // Alter only one final member, preserving its original witness/proof body.
    let operation = frames[1].operation_ref().unwrap();
    let key = config
        .keys
        .iter()
        .find(|key| key.scope == operation.header.extensions.scope)
        .unwrap();
    let plaintext = crate::wire::decrypt(
        &key.key,
        &encode(&operation.header.extensions).unwrap(),
        frames[1].body_bytes(),
    )
    .unwrap();
    let envelope: Envelope = crate::decode(&plaintext).unwrap();
    assert_eq!(envelope.version, 1);
    let changed = RowPayload::witness(
        RowPayload::reauthor_body(envelope.body, Some([9; 32])).unwrap(),
        envelope.creations,
    )
    .unwrap();
    let final_member = crate::wire::author(
        &node,
        operation.header.extensions.clone(),
        frames[1].chain().unwrap().clone(),
        &key.key,
        (
            operation.header.seq_num,
            operation.header.backlink,
            operation.header.timestamp,
        ),
        operation.header.previous.clone(),
        &changed,
    )
    .unwrap();
    assert_ne!(final_member.id().unwrap(), frames[1].id().unwrap());
    frames[1] = final_member;
    let reader = open(
        &f,
        &dir.path().join("reader.db"),
        &PrivateKey::from_bytes(&[109; 32]),
        f.full(),
        &both,
    )
    .await;
    // Commit actual custody intake, without running its later application pass.
    reader
        .stage_bundle_before_apply_for_test(&frames)
        .await
        .unwrap();
    assert_eq!(
        rows(
            &reader.inner.lock().await.conn,
            "SELECT status FROM secure_operations ORDER BY hash"
        )
        .await
        .unwrap(),
        vec![vec![SqlValue::Text("pending".into())]; 2]
    );
    let before = state(&reader).await;
    // The forwarding hook proves every real open_member gate succeeds first.
    assert_eq!(
        reader.apply_ready_bundle_for_test(&frames).await.unwrap(),
        "bundle members disagree on held resolution"
    );
    assert_eq!(state(&reader).await, before);
    for frame in &frames {
        assert_eq!(
            reader.receipt_status(frame.id().unwrap()).await.unwrap(),
            ReceiveStatus::Rejected
        );
    }
    assert_eq!(live(&reader, f.public).await, 0);
    assert_eq!(live(&reader, f.private).await, 0);
}
