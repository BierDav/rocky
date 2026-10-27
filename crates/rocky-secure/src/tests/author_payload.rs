//! One final authored frame keeps its first successful opening through storage.
use super::compaction::{Replica, follow_main, open};
use super::generations::{Lineage, select, session, text};
use super::*;
use crate::storage::rows;
use crate::wire::{Membership, RowPayload};
use rocky_core::{RowApplication, SqlValue};
use std::collections::BTreeMap;

fn start_trace() {
    crate::AUTHOR_PAYLOAD_TRACE.with(|trace| *trace.borrow_mut() = Some(Vec::new()));
}
fn trace() -> Vec<(EncryptedFrame, usize)> {
    crate::AUTHOR_PAYLOAD_TRACE.with(|trace| trace.borrow_mut().take().unwrap())
}
fn one_open(trace: &[(EncryptedFrame, usize)]) -> &EncryptedFrame {
    assert_eq!(trace.len(), 1);
    assert_eq!(trace[0].1, 1);
    &trace[0].0
}
fn plaintext(lineage: &Lineage, node: &PrivateKey, frame: &EncryptedFrame) -> Vec<u8> {
    let operation = frame.operation_ref().unwrap();
    let config = lineage.config(3, node);
    let key = config
        .keys
        .iter()
        .find(|key| key.scope == operation.header.extensions.scope)
        .unwrap();
    assert_eq!(operation.header.extensions.generation, key.generation);
    crate::wire::decrypt(
        &key.key,
        &encode(&operation.header.extensions).unwrap(),
        frame.body_bytes(),
    )
    .unwrap()
}
fn payload(lineage: &Lineage, node: &PrivateKey, frame: &EncryptedFrame) -> RowPayload {
    RowPayload::decode_test(&plaintext(lineage, node, frame), frame, &lineage.app(3)).unwrap()
}
async fn begin_two(sql: &crate::SecureSql<rocky_core::TypedRows>) -> Vec<i64> {
    sql.run("BEGIN", vec![]).await.unwrap();
    let mut identities = Vec::new();
    for name in ["one", "two"] {
        let id = sql.next_id("items").await.unwrap();
        identities.push(id.integer());
        sql.run(
            "INSERT INTO items(key,title,id) VALUES(?,?,?)",
            vec![text(name), text(name), SqlValue::Integer(id.integer())],
        )
        .await
        .unwrap();
    }
    identities
}
async fn records(replica: &Replica, query: &str) -> Vec<Vec<SqlValue>> {
    rows(
        &replica
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .conn,
        query,
    )
    .await
    .unwrap()
}
async fn snapshot(replica: &Replica) -> Vec<(String, Vec<Vec<SqlValue>>)> {
    super::compaction::audit::snapshot(replica).await
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Bundle {
    version: u8,
    proofs: BTreeMap<Commitment, crate::replica::moves::CreationProof>,
    carried: BTreeMap<Commitment, crate::replica::moves::Carried>,
    membership: Option<Membership>,
    reauthors: Option<Commitment>,
    writes: Vec<RowWrite>,
}

async fn refused_payload(earlier: u8) {
    let lineage = Lineage::pinned(193);
    let dir = TempDir::new().unwrap();
    let node = PrivateKey::from_bytes(&[194; 32]);
    let donor = open(&lineage, &dir, "donor", &node).await;
    let sql = session(&donor).await;
    begin_two(&sql).await;
    let frames = sql
        .commit([195; 32])
        .await
        .unwrap()
        .committed
        .unwrap()
        .frames;
    assert_eq!(frames.len(), 1);
    let donor_payload = payload(&lineage, &node, &frames[0]);
    let mut body: Bundle = crate::decode(&donor_payload.staging_body_for_test().unwrap()).unwrap();
    assert_eq!(body.version, 5);
    let valid = encode(&body).unwrap();
    body.version = 6;
    let invalid = encode(&body).unwrap();
    let mut writes = body.writes.clone();
    if earlier == 1 {
        // Valid images and identities, but an invalid original publication mask.
        writes[1].assigned.insert([99; 32]);
    }
    let replica = open(&lineage, &dir, "author", &node).await;
    follow_main(&lineage, &replica).await;
    let clean = snapshot(&replica).await;
    let scope = lineage.scope("items");
    if earlier == 2 {
        // Privileged corruption fixture, restored before any audit.
        let author = lineage
            .app(3)
            .row_identity(&writes[0].change)
            .unwrap()
            .unwrap()
            .author();
        replica
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .conn
            .execute(
                "INSERT INTO secure_identity_own VALUES(?,?,?)",
                (encode(&scope).unwrap(), i64::from(author), -1_i64),
            )
            .await
            .unwrap();
    }
    let before = snapshot(&replica).await;
    let revision = replica.stored_and_memory_revision().await;
    let context = replica.schema_context().await;
    start_trace();
    let error = replica
        .author_payload_for_test(&writes, &invalid)
        .await
        .unwrap_err();
    let seen = trace();
    assert_eq!(seen.len(), usize::from(earlier != 1));
    if let Some((_, openings)) = seen.first() {
        assert_eq!(*openings, usize::from(earlier == 0));
    }
    match earlier {
        0 => assert!(
            matches!(error, Error::Rejected(ref reason) if reason == "unsupported bundle version"),
            "{error}"
        ),
        1 => assert!(
            matches!(error, Error::Rejected(ref reason) if reason == "row/field transition denied; permission fields never change by row writes"),
            "{error}"
        ),
        2 => assert!(
            matches!(error, Error::Corrupt(ref reason) if reason == "summary sequence"),
            "{error}"
        ),
        _ => unreachable!(),
    }
    assert_eq!(snapshot(&replica).await, before);
    assert_eq!(replica.stored_and_memory_revision().await, revision);
    assert_eq!(replica.schema_context().await, context);
    if earlier == 2 {
        replica
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .conn
            .execute(
                "DELETE FROM secure_identity_own WHERE scope=?",
                [encode(&scope).unwrap()],
            )
            .await
            .unwrap();
    }
    assert_eq!(snapshot(&replica).await, clean);
    replica.audit(scope).await.unwrap();
    start_trace();
    let control = replica
        .author_payload_for_test(&body.writes, &valid)
        .await
        .unwrap();
    let seen = trace();
    assert_eq!(one_open(&seen).id().unwrap(), control.id().unwrap());
    assert_eq!(payload(&lineage, &node, &control).writes(), body.writes);
    replica.audit(scope).await.unwrap();
}

#[tokio::test]
async fn unsupported_final_author_payload_refuses_without_authoritative_changes() {
    refused_payload(0).await;
}
#[tokio::test]
async fn original_write_denial_precedes_the_final_author_payload_version() {
    refused_payload(1).await;
}
#[tokio::test]
async fn live_own_marks_error_precedes_the_final_author_payload_version() {
    refused_payload(2).await;
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Proof<B, H> {
    version: u8,
    #[serde(with = "serde_bytes")]
    manifest: Commitment,
    #[serde(with = "serde_bytes")]
    table: Commitment,
    id: rocky_core::RowIdentity,
    #[serde(with = "serde_bytes")]
    provenance: Commitment,
    stamp: crate::replica::Stamp,
    bindings: B,
    signer: crate::replica::AuthorKey,
    header: H,
}
#[derive(serde::Deserialize)]
struct StoredRow {
    proof: Proof<serde_bytes::ByteArray<32>, serde_bytes::ByteArray<32>>,
    fields: BTreeMap<serde_bytes::ByteArray<32>, serde_bytes::ByteBuf>,
    stamps: BTreeMap<serde_bytes::ByteArray<32>, crate::replica::Stamp>,
    creation: Hash,
    identity: rocky_core::RowIdentity,
    key: serde_bytes::ByteArray<32>,
    created: crate::replica::Stamp,
}

async fn check_births(
    lineage: &Lineage,
    node: &PrivateKey,
    replica: &Replica,
    frame: &EncryptedFrame,
    ids: &[i64],
) {
    let operation = frame.operation_ref().unwrap();
    let opened = payload(lineage, node, frame);
    assert_eq!(operation.header.extensions.scope, lineage.scope("items"));
    assert_eq!(operation.header.public_key, node.public_key());
    let membership = opened.membership().unwrap();
    assert_eq!(membership.index, 0);
    assert_eq!(membership.members.len(), 1);
    assert_eq!(opened.reauthors(), None);
    assert!(opened.creations().is_empty());
    assert_eq!(opened.writes().len(), 2);
    let stamp = crate::replica::Stamp {
        clock: operation.header.timestamp,
        author: operation.header.extensions.author.unwrap_or(0),
        sequence: operation.header.extensions.author_sequence,
    };
    let stored_bytes = records(
        replica,
        "SELECT fields FROM secure_row_records ORDER BY row",
    )
    .await;
    let mut stored = Vec::new();
    {
        let inner = replica.inner.lock(crate::replica::Class::Background).await;
        for row in stored_bytes {
            let SqlValue::Blob(bytes) = &row[0] else {
                panic!("stored row");
            };
            stored.push(vec![SqlValue::Blob(
                encode(&inner.decode_record(bytes).await.unwrap()).unwrap(),
            )]);
        }
    }
    let schema = lineage.app(3);
    let mut seen_ids = Vec::new();
    let mut claims = Vec::new();
    for (ordinal, write) in opened.writes().iter().enumerate() {
        assert!(write.change.before.is_none());
        assert!(write.assigned.is_empty());
        assert_eq!(
            write.published_fields(),
            write
                .change
                .after
                .as_ref()
                .unwrap()
                .keys()
                .copied()
                .collect()
        );
        let identity = schema.row_identity(&write.change).unwrap().unwrap();
        seen_ids.push(identity.integer());
        assert_eq!(Some(identity.author()), operation.header.extensions.author);
        let proof = opened.proof(&write.change.row).unwrap();
        assert!(opened.carried(&write.change.row).is_none());
        assert_eq!(proof.stamp(), &stamp);
        proof
            .verify(
                lineage.publisher().public_key(),
                lineage.definition(3).hash(),
                lineage.scope("items").table,
                identity,
                &stamp,
            )
            .unwrap();
        let original: Proof<Vec<rocky_credentials::AuthorBinding>, serde_bytes::ByteBuf> =
            crate::decode(&encode(proof).unwrap()).unwrap();
        let binding = records(
            replica,
            "SELECT digest,bindings FROM secure_author_bindings",
        )
        .await;
        assert_eq!(binding.len(), 1);
        let SqlValue::Blob(bytes) = &binding[0][1] else {
            panic!("bindings")
        };
        assert_eq!(bytes, &encode(&original.bindings).unwrap());
        let digest = rocky_core::commitment("rocky.author-bindings.v1", bytes);
        assert_eq!(binding[0][0], SqlValue::Blob(digest.to_vec()));
        let provenance = rocky_core::CreationStatement::derived(
            original.manifest,
            original.table,
            original.id,
            original.stamp.clone(),
            digest,
        )
        .commitment();
        claims.push(vec![
            SqlValue::Integer(ordinal as i64),
            SqlValue::Integer(identity.integer()),
            SqlValue::Integer(0),
            SqlValue::Blob(provenance.to_vec()),
            SqlValue::Blob(operation.hash.as_bytes().to_vec()),
        ]);
        let expected = Proof {
            version: original.version,
            manifest: original.manifest,
            table: original.table,
            id: original.id,
            provenance: original.provenance,
            stamp: original.stamp,
            bindings: serde_bytes::ByteArray::new(digest),
            signer: original.signer,
            header: serde_bytes::ByteArray::new(rocky_core::commitment(
                "rocky.creation-header.v1",
                &original.header,
            )),
        };
        let record = stored
            .iter()
            .find_map(|row| {
                let SqlValue::Blob(bytes) = &row[0] else {
                    panic!("row")
                };
                let record: StoredRow = p2panda_core::cbor::decode_cbor(bytes.as_slice()).unwrap();
                (record.identity == identity).then_some(record)
            })
            .unwrap();
        assert_eq!(encode(&record.proof).unwrap(), encode(&expected).unwrap());
        assert_eq!(record.creation, operation.hash);
        assert_eq!(record.created, stamp);
        assert_eq!(record.key.as_ref(), &write.change.row);
        assert_eq!(
            record.fields,
            write
                .change
                .after
                .as_ref()
                .unwrap()
                .iter()
                .map(|(key, value)| (
                    serde_bytes::ByteArray::new(*key),
                    serde_bytes::ByteBuf::from(value.clone())
                ))
                .collect()
        );
        assert_eq!(
            record.stamps,
            write
                .published_fields()
                .iter()
                .map(|field| (serde_bytes::ByteArray::new(*field), stamp.clone()))
                .collect()
        );
    }
    assert_eq!(seen_ids, ids);
    assert_eq!(
        records(replica, "SELECT c.ordinal,c.identity,c.kind,c.provenance,e.frame FROM secure_admission_claims c JOIN secure_admission_entries e ON e.entry=c.entry ORDER BY c.ordinal").await,
        claims
    );
    assert_eq!(
        records(
            replica,
            "SELECT ordinal FROM secure_admission_claims ORDER BY ordinal"
        )
        .await,
        vec![vec![SqlValue::Integer(0)], vec![SqlValue::Integer(1)]]
    );
    assert_eq!(
        records(replica, "SELECT count(*) FROM secure_admission_entries").await,
        vec![vec![SqlValue::Integer(1)]]
    );
    for table in [
        "secure_identity_marks",
        "secure_identity_own",
        "secure_identity_ledger",
    ] {
        assert_eq!(
            records(replica, &format!("SELECT max(sequence) FROM {table}")).await,
            vec![vec![SqlValue::Integer(2)]]
        );
    }
    assert_eq!(
        select(replica, "SELECT key,title FROM items ORDER BY key").await,
        vec![
            vec![text("one"), text("one")],
            vec![text("two"), text("two")]
        ]
    );
}

#[tokio::test]
async fn author_retains_complete_two_birth_payload_and_opens_the_next_update_afresh() {
    let lineage = Lineage::pinned(196);
    let dir = TempDir::new().unwrap();
    let node = PrivateKey::from_bytes(&[197; 32]);
    let replica = open(&lineage, &dir, "author", &node).await;
    follow_main(&lineage, &replica).await;
    let sql = session(&replica).await;
    let ids = begin_two(&sql).await;
    start_trace();
    let committed = sql.commit([198; 32]).await.unwrap().committed.unwrap();
    let seen = trace();
    assert_eq!(committed.frames.len(), 1);
    assert_eq!(
        one_open(&seen).id().unwrap(),
        committed.frames[0].id().unwrap()
    );
    check_births(&lineage, &node, &replica, &committed.frames[0], &ids).await;
    start_trace();
    let update = sql
        .run("UPDATE items SET title='updated' WHERE key='one'", vec![])
        .await
        .unwrap()
        .committed
        .unwrap();
    let seen = trace();
    assert_eq!(update.frames.len(), 1);
    assert_eq!(
        one_open(&seen).id().unwrap(),
        update.frames[0].id().unwrap()
    );
    assert_ne!(seen[0].0.id().unwrap(), committed.frames[0].id().unwrap());
    let opened = payload(&lineage, &node, &update.frames[0]);
    assert_eq!(opened.writes().len(), 1);
    let write = &opened.writes()[0];
    assert!(write.change.before.is_some() && write.change.after.is_some());
    assert!(opened.proof(&write.change.row).is_none());
    let incarnation = rocky_core::commitment("rocky.row-incarnation.v2", &ids[0].to_le_bytes());
    assert_eq!(
        opened.creations(),
        &[(incarnation, *committed.frames[0].id().unwrap().as_bytes())]
    );
    replica.audit(lineage.scope("items")).await.unwrap();
    drop(sql);
    drop(replica);
    let reopened = lineage
        .reopen(&dir.path().join("author.db"), &node, 3, 3)
        .await
        .unwrap();
    assert_eq!(
        select(&reopened, "SELECT id,title FROM items ORDER BY key").await,
        vec![
            vec![SqlValue::Integer(ids[0]), text("updated")],
            vec![SqlValue::Integer(ids[1]), text("two")]
        ]
    );
    reopened.audit(lineage.scope("items")).await.unwrap();
}

#[tokio::test]
async fn late_sql_commit_failure_discards_the_author_payload_and_same_handle_retries_once() {
    let lineage = Lineage::pinned(199);
    let dir = TempDir::new().unwrap();
    let node = PrivateKey::from_bytes(&[200; 32]);
    let replica = open(&lineage, &dir, "author", &node).await;
    follow_main(&lineage, &replica).await;
    let before = snapshot(&replica).await;
    let revision = replica.stored_and_memory_revision().await;
    let context = replica.schema_context().await;
    let mut observer = replica.observe_revision();
    let mut tables = replica.observe_tables(Some(&["items"])).await.unwrap();
    replica.fail_next_sql_commit().await;
    let sql = session(&replica).await;
    let ids = begin_two(&sql).await;
    start_trace();
    assert!(matches!(
        sql.commit([201; 32]).await,
        Err(Error::InjectedFailure)
    ));
    let failed = trace();
    one_open(&failed);
    assert_eq!(snapshot(&replica).await, before);
    assert_eq!(replica.stored_and_memory_revision().await, revision);
    assert_eq!(replica.schema_context().await, context);
    assert!(!observer.has_changed().unwrap());
    assert_eq!(tables.try_next().unwrap(), None);
    let retry_ids = begin_two(&sql).await;
    assert_eq!(retry_ids, ids);
    start_trace();
    let committed = sql.commit([201; 32]).await.unwrap().committed.unwrap();
    let retried = trace();
    assert_eq!(
        one_open(&retried).id().unwrap(),
        committed.frames[0].id().unwrap()
    );
    check_births(&lineage, &node, &replica, &committed.frames[0], &ids).await;
    assert!(observer.has_changed().unwrap());
    observer.borrow_and_update();
    assert!(tables.try_next().unwrap().is_some());
    let committed_state = snapshot(&replica).await;
    let original = payload(&lineage, &node, &committed.frames[0]).into_writes();
    start_trace();
    let duplicate = replica
        .author_bundle([201; 32], original, vec![])
        .await
        .unwrap();
    assert!(trace().is_empty());
    assert_eq!(committed.handle, [201; 32]);
    assert_eq!(
        duplicate
            .iter()
            .map(|frame| frame.id().unwrap())
            .collect::<Vec<_>>(),
        committed
            .frames
            .iter()
            .map(|frame| frame.id().unwrap())
            .collect::<Vec<_>>()
    );
    assert_eq!(snapshot(&replica).await, committed_state);
    assert!(!observer.has_changed().unwrap());
    assert_eq!(tables.try_next().unwrap(), None);
    replica.audit(lineage.scope("items")).await.unwrap();
    drop(sql);
    drop(replica);
    let reopened = lineage
        .reopen(&dir.path().join("author.db"), &node, 3, 3)
        .await
        .unwrap();
    check_births(&lineage, &node, &reopened, &committed.frames[0], &ids).await;
    reopened.audit(lineage.scope("items")).await.unwrap();
}
