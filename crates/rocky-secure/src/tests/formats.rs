use super::*;
use crate::storage::rows;
use std::collections::BTreeMap;

#[tokio::test]
async fn unsupported_replica_and_checkpoint_formats_refuse_before_writes() {
    let f = Fixture::new();
    let template = TempDir::new().unwrap();
    let source = template.path().join("template.sqlite");
    let node = PrivateKey::new();
    let replica = EncryptedReplica::open(&source, f.app(), f.config(&node, f.full(), &[f.public]))
        .await
        .unwrap();
    replica.checkpoint(f.public).await.unwrap();
    replica
        .backup_into(&template.path().join("base.sqlite"))
        .await
        .unwrap();
    drop(replica);
    let source = template.path().join("base.sqlite");
    for version in [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 15] {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("replica.sqlite");
        std::fs::copy(&source, &path).unwrap();
        let replica =
            EncryptedReplica::open(&path, f.app(), f.config(&node, f.full(), &[f.public]))
                .await
                .unwrap();
        {
            let mut inner = replica.inner.lock().await;
            let records = rows(
                &inner.conn,
                "SELECT signed FROM secure_checkpoints WHERE signed IS NOT NULL",
            )
            .await
            .unwrap();
            let [rocky_core::SqlValue::Blob(bytes)] = records[0].as_slice() else {
                panic!("checkpoint");
            };
            let mut signed = SignedCheckpoint::from_bytes(bytes).unwrap();
            signed.statement.format = version;
            inner
                .conn
                .execute(
                    "UPDATE secure_checkpoints SET signed=? WHERE signed IS NOT NULL",
                    [encode(&signed).unwrap()],
                )
                .await
                .unwrap();
            inner.flush().unwrap();
        }
        drop(replica);
        let before = std::fs::read(&path).unwrap();
        let error = EncryptedReplica::open(&path, f.app(), f.config(&node, f.full(), &[f.public]))
            .await
            .err()
            .unwrap();
        assert!(
            error.to_string().contains("unsupported checkpoint format"),
            "{error}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
    for version in [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
        26,
    ] {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("replica.sqlite");
        std::fs::copy(&source, &path).unwrap();
        let replica =
            EncryptedReplica::open(&path, f.app(), f.config(&node, f.full(), &[f.public]))
                .await
                .unwrap();
        {
            let mut inner = replica.inner.lock().await;
            inner
                .conn
                .execute(
                    "UPDATE secure_metadata SET identity=?",
                    [encode(&(
                        format!("rocky.encrypted-replica.v{version}"),
                        rocky_credentials::genesis_tip(f.app().definition().hash()),
                        node.public_key(),
                    ))
                    .unwrap()],
                )
                .await
                .unwrap();
            inner.flush().unwrap();
        }
        drop(replica);
        let before = std::fs::read(&path).unwrap();
        let error = EncryptedReplica::open(&path, f.app(), f.config(&node, f.full(), &[f.public]))
            .await
            .err()
            .unwrap();
        assert!(
            error
                .to_string()
                .contains("unsupported encrypted replica format"),
            "{error}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[tokio::test]
async fn unsupported_frames_leave_operation_projection_and_revision_unchanged() {
    let f = Fixture::new();
    let directory = TempDir::new().unwrap();
    let node = PrivateKey::new();
    let replica = EncryptedReplica::open(
        directory.path().join("replica.sqlite"),
        f.app(),
        f.config(&node, f.full(), &[f.public]),
    )
    .await
    .unwrap();
    let change = RowChange {
        scope: f.public,
        row: [7; 32],
        before: None,
        after: Some(f.app().name("kept")),
    };
    let frame = replica
        .author([1; 32], vec![change], Vec::new())
        .await
        .unwrap();
    let before = replica.snapshot(f.public).await.unwrap();
    let operations = rows(
        &replica.inner.lock().await.conn,
        "SELECT hash FROM secure_operations",
    )
    .await
    .unwrap();
    for version in [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 12] {
        let mut operation = frame.operation().unwrap();
        operation.header.extensions.row_format = version;
        operation.header.sign(&node);
        let altered = frame.with_header(operation.header.to_bytes());
        let error = replica.receive(altered).await.unwrap_err();
        assert!(
            error.to_string().contains("unsupported row frame format"),
            "{error}"
        );
        let after = replica.snapshot(f.public).await.unwrap();
        assert_eq!(
            (after.revision, after.rows, after.complete),
            (before.revision, before.rows.clone(), before.complete)
        );
        assert_eq!(
            rows(
                &replica.inner.lock().await.conn,
                "SELECT hash FROM secure_operations"
            )
            .await
            .unwrap(),
            operations
        );
    }
}

#[tokio::test]
async fn unsupported_payload_formats_refuse_before_custody_writes() {
    #[derive(Serialize)]
    struct Body {
        version: u8,
        proofs: BTreeMap<Commitment, crate::replica::moves::CreationProof>,
        carried: BTreeMap<Commitment, crate::replica::moves::Carried>,
        membership: Option<crate::wire::Membership>,
        writes: Vec<RowWrite>,
    }
    #[derive(Serialize)]
    struct Envelope {
        version: u8,
        #[serde(with = "serde_bytes")]
        body: Vec<u8>,
        creations: Vec<(Commitment, Commitment)>,
        basis: Option<(u64, Commitment)>,
        releases: Vec<Commitment>,
    }
    let f = Fixture::new();
    let directory = TempDir::new().unwrap();
    let node = PrivateKey::new();
    let replica = EncryptedReplica::open(
        directory.path().join("replica.sqlite"),
        f.app(),
        f.config(&node, f.full(), &[f.public]),
    )
    .await
    .unwrap();
    let sender = PrivateKey::new();
    let chain = crate::wire::Chain::new(f.credentials(&sender, f.full())).unwrap();
    let extensions = crate::wire::Extensions {
        tip: rocky_credentials::genesis_tip(f.app().definition().hash()),
        scope: f.public,
        generation: rocky_core::KeyEpoch::new(1, &[11; 32]),
        chain: chain.digest(),
        author: Some(
            f.config(&sender, f.full(), &[f.public])
                .credentials
                .author_number()
                .unwrap(),
        ),
        author_sequence: 0,
        row_format: crate::wire::ROW_FORMAT,
        partition: None,
        existence: 0,
    };
    for (envelope_version, body_version, message) in [
        (0, 5, "unsupported creation witness format"),
        (2, 5, "unsupported creation witness format"),
        (1, 0, "unsupported bundle version"),
        (1, 1, "unsupported bundle version"),
        (1, 2, "unsupported bundle version"),
        (1, 3, "unsupported bundle version"),
        (1, 4, "unsupported bundle version"),
        (1, 6, "unsupported bundle version"),
    ] {
        let plaintext = encode(&Envelope {
            version: envelope_version,
            body: encode(&Body {
                version: body_version,
                proofs: BTreeMap::new(),
                carried: BTreeMap::new(),
                membership: None,
                writes: vec![],
            })
            .unwrap(),
            creations: vec![],
            basis: None,
            releases: vec![],
        })
        .unwrap();
        let frame = crate::wire::author(
            &sender,
            extensions.clone(),
            chain.clone(),
            &[11; 32],
            (0, None, 1),
            vec![],
            &plaintext,
        )
        .unwrap();
        let error = replica.receive(frame).await.unwrap_err();
        assert!(error.to_string().contains(message), "{error}");
        assert!(
            rows(
                &replica.inner.lock().await.conn,
                "SELECT hash FROM secure_operations"
            )
            .await
            .unwrap()
            .is_empty()
        );
        let snapshot = replica.snapshot(f.public).await.unwrap();
        assert_eq!(snapshot.revision, 0);
        assert!(snapshot.rows.is_empty());
    }
}

#[tokio::test]
async fn seal_author_positions_span_resource_logs_and_survive_reopen() {
    let f = Fixture::new();
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("replica.sqlite");
    let node = PrivateKey::new();
    let replica = EncryptedReplica::open(
        &path,
        f.app(),
        f.config(&node, f.full(), &[f.public, f.private]),
    )
    .await
    .unwrap();
    let first = replica
        .author(
            [31; 32],
            vec![change(f.public, 31, None, Some(f.app().name("first")))],
            vec![],
        )
        .await
        .unwrap();
    let second = replica
        .author(
            [32; 32],
            vec![change(f.private, 32, None, Some(f.app().salary("42")))],
            vec![],
        )
        .await
        .unwrap();
    let a = first.operation().unwrap().header;
    let b = second.operation().unwrap().header;
    assert_eq!((a.seq_num, b.seq_num), (0, 0));
    assert_eq!(
        (a.extensions.author_sequence, b.extensions.author_sequence),
        (0, 1)
    );
    drop(replica);
    let replica = EncryptedReplica::open(
        &path,
        f.app(),
        f.config(&node, f.full(), &[f.public, f.private]),
    )
    .await
    .unwrap();
    let third = replica
        .author(
            [33; 32],
            vec![change(f.public, 33, None, Some(f.app().name("third")))],
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(
        third.operation().unwrap().header.extensions.author_sequence,
        2
    );
}
