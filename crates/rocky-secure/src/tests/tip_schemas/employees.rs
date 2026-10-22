//! Same-definition holding borrows the original compiled callbacks and ACL policy.
use super::*;
use rocky_core::{ApplicationManifest, RowApplication};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

struct Probe {
    app: Employees,
    calls: Arc<AtomicUsize>,
    refuse: bool,
}
impl RowApplication for Probe {
    fn manifest(&self) -> ApplicationManifest {
        self.app.manifest()
    }
    fn permission_fields(&self) -> BTreeSet<Commitment> {
        self.app.permission_fields()
    }
    fn validate_changes(&self, _changes: &[RowChange]) -> Result<(), String> {
        Err("held validation must not invent a singleton batch".into())
    }
    fn validate_write(&self, _write: &RowWrite) -> Result<(), String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.refuse {
            Err("original compiled callback refused".into())
        } else {
            Ok(())
        }
    }
}

#[tokio::test]
async fn employees_held_originals_keep_callbacks_and_the_current_acl_guard() {
    let f = Fixture::with_publisher(PrivateKey::from_bytes(&[193; 32]));
    let dir = TempDir::new().unwrap();
    let author = PrivateKey::from_bytes(&[194; 32]);
    let source = EncryptedReplica::open(
        dir.path().join("source"),
        f.app(),
        f.config(&author, f.full(), &[f.public, f.private]),
    )
    .await
    .unwrap();
    let original = source
        .author(
            [147; 32],
            vec![change(f.public, 1, None, Some(f.app().name("Original")))],
            vec![],
        )
        .await
        .unwrap();
    let operation = original.operation_ref().unwrap();
    let bytes = crate::wire::decrypt(
        &[11; 32],
        &encode(&operation.header.extensions).unwrap(),
        original.body_bytes(),
    )
    .unwrap();
    let payload = crate::wire::RowPayload::decode_test(&bytes, &original, &f.app()).unwrap();
    let mut writes = payload.writes().to_vec();
    writes[0]
        .change
        .after
        .as_mut()
        .unwrap()
        .insert(f.app().field("owner"), b"owner change".to_vec());
    let acl = super::source_validation::reseal(
        &original,
        &author,
        &[11; 32],
        original.chain().unwrap().clone(),
        writes,
        &payload,
    );
    for mode in 0..3 {
        let key = PrivateKey::from_bytes(&[195 + mode; 32]);
        let calls = Arc::new(AtomicUsize::new(0));
        let target = EncryptedReplica::open(
            dir.path().join(format!("target{mode}")),
            Probe {
                app: f.app(),
                calls: calls.clone(),
                refuse: mode == 1,
            },
            f.config(&key, f.full(), &[f.public, f.private]),
        )
        .await
        .unwrap();
        let statements = super::source_validation::closed_statements(
            f.public.domain,
            target.followed_tip().await,
            f.app().manifest().hash(),
            &f.publisher,
        );
        target.receive_tip_statements(&statements).await.unwrap();
        target
            .follow_bookmark(f.public.domain, "main")
            .await
            .unwrap();
        let result = target
            .receive_held_frames(vec![if mode == 2 {
                acl.clone()
            } else {
                original.clone()
            }])
            .await;
        match mode {
            0 => {
                result.unwrap();
                assert_eq!(calls.load(Ordering::SeqCst), 1);
                assert_eq!(target.held_changes().await.unwrap().len(), 1);
            }
            1 => {
                assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .contains("original compiled callback refused")
                );
                assert_eq!(calls.load(Ordering::SeqCst), 1);
            }
            _ => {
                assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .contains("held row/field transition denied")
                );
                assert_eq!(calls.load(Ordering::SeqCst), 0);
            }
        }
    }
}
