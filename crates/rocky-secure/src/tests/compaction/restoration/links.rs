//! Signed retained-store corruptions still require exact original-node links.
use super::negatives::{filled, preserved};
use super::*;
use crate::storage::bindings;

#[tokio::test]
async fn resigned_clock_scope_signer_and_unconnected_backlinks_never_complete() {
    let f = filled().await;
    let scope = f.lineage.scope("items");
    let original = f.frames[2].operation_ref().unwrap().header.clone();
    let original_hash = original.hash();
    let mut clock = original.clone();
    clock.timestamp = f.frames[1].operation_ref().unwrap().header.timestamp;
    clock.sign(&f.key);
    let mut fork = original.clone();
    fork.backlink = Some(Hash::new(b"a predecessor outside the retained prefix"));
    fork.sign(&f.key);
    let mut other_scope = original.clone();
    other_scope.extensions.scope.encryption[0] ^= 1;
    other_scope.sign(&f.key);
    let current_line = PrivateKey::from_bytes(&[185; 32]);
    let mut other_signer = original.clone();
    other_signer.public_key = current_line.public_key();
    other_signer.sign(&current_line);
    let stranger = PrivateKey::from_bytes(&[186; 32]);
    let mut other_original = original.clone();
    other_original.public_key = stranger.public_key();
    other_original.sign(&stranger);
    let mut source = original.clone();
    source.extensions.tip = rocky_core::TipId([99; 32]);
    source.sign(&f.key);
    let mut author_number = original.clone();
    author_number.extensions.author = Some(original.extensions.author.unwrap() + 1);
    author_number.sign(&f.key);
    let mut bad_signature = original.clone();
    bad_signature.sign(&current_line);
    let mut partition = original.clone();
    partition.extensions.partition = Some(
        rocky_core::PartitionTuple::new([(
            [1; 32],
            rocky_core::PartitionValue::new(
                rocky_core::KeyColumn {
                    affinity: rocky_core::Affinity::Text,
                    collation: rocky_core::Collation::Binary,
                },
                text("another partition"),
            )
            .unwrap(),
        )])
        .unwrap(),
    );
    partition.sign(&f.key);
    // These are privileged signed mutations, not admissible receive examples.
    for (header, error) in [
        (clock, true),
        (source, true),
        (partition, true),
        (author_number, true),
        (bad_signature, true),
        (fork, false),
        (other_scope, true),
        (other_signer, true),
        (other_original, true),
    ] {
        let hash = header.hash();
        f.replica
            .inner
            .lock()
            .await
            .conn
            .execute(
                "UPDATE secure_operations SET hash=?,author=?,scope=?,header=? WHERE hash=?",
                bindings(vec![
                    SqlValue::Blob(hash.as_bytes().to_vec()),
                    SqlValue::Blob(header.public_key.as_bytes().to_vec()),
                    SqlValue::Blob(crate::encode(&header.extensions.scope).unwrap()),
                    SqlValue::Blob(header.to_bytes()),
                    SqlValue::Blob(original_hash.as_bytes().to_vec()),
                ]),
            )
            .await
            .unwrap();
        f.replica
            .inner
            .lock()
            .await
            .conn
            .execute(
                "UPDATE secure_evicted_logs SET hash=? WHERE scope=?",
                [hash.as_bytes().to_vec(), crate::encode(&scope).unwrap()],
            )
            .await
            .unwrap();
        preserved(&f, error).await;
        f.replica
            .inner
            .lock()
            .await
            .conn
            .execute(
                "UPDATE secure_operations SET hash=?,author=?,scope=?,header=? WHERE hash=?",
                bindings(vec![
                    SqlValue::Blob(original_hash.as_bytes().to_vec()),
                    SqlValue::Blob(original.public_key.as_bytes().to_vec()),
                    SqlValue::Blob(crate::encode(&scope).unwrap()),
                    SqlValue::Blob(original.to_bytes()),
                    SqlValue::Blob(hash.as_bytes().to_vec()),
                ]),
            )
            .await
            .unwrap();
        f.replica
            .inner
            .lock()
            .await
            .conn
            .execute(
                "UPDATE secure_evicted_logs SET hash=? WHERE scope=?",
                [
                    original_hash.as_bytes().to_vec(),
                    crate::encode(&scope).unwrap(),
                ],
            )
            .await
            .unwrap();
    }
    f.replica.restored(scope).await.unwrap();
    f.replica.audit(scope).await.unwrap();
}
