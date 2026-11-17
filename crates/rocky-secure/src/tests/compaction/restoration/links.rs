use super::negatives::{filled, preserved};
use super::*;
use crate::storage::bindings;

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
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
    let mut bad_signature = original.clone();
    bad_signature.sign(&current_line);
    let mut other_partition = original.clone();
    other_partition.extensions.scope.encryption = [3; 32];
    other_partition.sign(&f.key);
    let stored_directly_not_received = [
        (clock, true),
        (source, true),
        (other_partition, true),
        (bad_signature, true),
        (fork, false),
        (other_scope, true),
        (other_signer, true),
        (other_original, true),
    ];
    for (header, error) in stored_directly_not_received {
        let hash = header.hash();
        f.replica
            .inner
            .lock(crate::replica::Class::Background)
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
            .lock(crate::replica::Class::Background)
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
            .lock(crate::replica::Class::Background)
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
            .lock(crate::replica::Class::Background)
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
