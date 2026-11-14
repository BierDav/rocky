//! Current-format byte fixtures captured on the software backend.
use super::*;
use rocky_core::{AppId, DomainId, MeshId, ResourceGrant, Scope};
use rocky_credentials::{Authority, device_chain};

fn materialized(deleted: bool) -> Materialized {
    let signer = PrivateKey::from_bytes(&[42; 32]).public_key();
    let stamp = Stamp {
        clock: 7,
        author: writer(&signer),
        sequence: 3,
    };
    Materialized {
        proof: None,
        moved_from: None,
        fields: if deleted {
            BTreeMap::new()
        } else {
            [([5; 32], vec![0, 255, 7])].into()
        },
        stamps: [([5; 32], stamp.clone())].into(),
        authors: [(writer(&signer), signer)].into(),
        deleted,
        creation: Hash::from_bytes([6; 32]),
        identity: None,
        key: None,
        created: Some(stamp),
    }
}
fn signed() -> SignedCheckpoint {
    let owner = PrivateKey::from_bytes(&[40; 32]);
    let user = PrivateKey::from_bytes(&[41; 32]);
    let node = PrivateKey::from_bytes(&[42; 32]);
    let scope = ResourceScope {
        domain: Scope {
            mesh: MeshId([1; 16]),
            app: AppId([2; 16]),
            domain: DomainId([3; 16]),
        },
        encryption: [4; 32],
        table: [5; 32],
    };
    let grant = ResourceGrant {
        scope,
        rows: RowSelection::All,
        fields: rocky_core::FieldSelection::All,
        rights: [Right::Read, Right::Insert, Right::Update, Right::Delete].into(),
    };
    let root = Authority::owner(&owner)
        .issue(
            user.public_key(),
            vec![grant],
            rocky_credentials::Validity {
                issued_at: 1_800_000_000,
                lifetime: std::time::Duration::from_secs(86400),
                clock_skew: std::time::Duration::from_secs(0),
            },
        )
        .unwrap();
    let credentials = device_chain(root, &user, node.public_key()).unwrap();
    credentials
        .verify(
            owner.public_key(),
            node.public_key(),
            &RevocationSet::new(),
            1_800_000_001,
        )
        .unwrap();
    let mut sets = Sets::new();
    sets.replace(&[8; 32], None, &encode(&materialized(false)).unwrap())
        .unwrap();
    sets.create(Hash::from_bytes([6; 32]), 7);
    let mut handles = Lt::new();
    handles.add(HANDLE_LABEL, &handle_item(&[9; 32], &[10; 32], &[11; 32]));
    SignedCheckpoint::sign(
        CheckpointStatement {
            format: CHECKPOINT_FORMAT,
            seal: Some([12; 32]),
            partition: None,
            identities: vec![CoveredMark {
                author: writer(&node.public_key()),
                signer: node.public_key(),
                sequence: 9,
                own: 3,
            }],
            moves: SetSummary {
                count: 0,
                digest: [0; 32],
            },
            tip: rocky_core::TipId([13; 32]),
            schema: 2,
            revocations: RevocationSet::new().digest(1_800_000_001).unwrap(),
            scope,
            state: CheckpointState::Keyed {
                key_epoch: rocky_core::KeyEpoch::new(2, &[3; 32]),
            },
            sequence: 2,
            previous: Some([14; 32]),
            frontier: vec![LogPosition {
                author: node.public_key(),
                sequence: 3,
                hash: Hash::from_bytes([15; 32]),
                clock: 7,
            }],
            clock: 7,
            operations: 4,
            rows: sets.rows_summary(),
            tombstones: 0,
            creations: sets.creations_summary(),
            handles: SetSummary {
                count: 1,
                digest: handles.digest(),
            },
            custody: CustodySummary {
                superseded: 4,
                sole: SetSummary {
                    count: 0,
                    digest: Lt::new().digest(),
                },
            },
            outcomes: SealedOutcomes {
                listed: vec![
                    SealedTransaction {
                        stamp: Stamp {
                            clock: 5,
                            author: writer(&node.public_key()),
                            sequence: 1,
                        },
                        transaction: [16; 32],
                        outcome: SealedOutcome::Dropped {
                            reason: rocky_core::replay::DropReason::Constraint,
                            violation: Some("UNIQUE constraint failed: items.name".into()),
                        },
                    },
                    SealedTransaction {
                        stamp: Stamp {
                            clock: 6,
                            author: writer(&node.public_key()),
                            sequence: 2,
                        },
                        transaction: [17; 32],
                        outcome: SealedOutcome::Dropped {
                            reason: rocky_core::replay::DropReason::Admission,
                            violation: None,
                        },
                    },
                ],
                horizon: None,
            },
        },
        &node,
        credentials,
    )
    .unwrap()
}
macro_rules! frozen {
    ($name:literal) => {
        include_bytes!(concat!("backend_vectors/", $name)).as_slice()
    };
}
fn check_lt(actual: &Lt, name: &str, (state, digest): (&[u8], &[u8])) {
    if let Some(directory) = updating() {
        rewrite(&directory, format!("lt-{name}.bin"), actual.to_bytes());
        rewrite(&directory, format!("lt-{name}-digest.bin"), actual.digest());
        return;
    }
    assert_eq!(actual.to_bytes(), state);
    assert_eq!(actual.digest().as_slice(), digest);
    assert!(Lt::from_bytes(state).unwrap() == *actual);
}
#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn full_lanes_keep_independent_bytes_and_wrapping() {
    let row = [[8; 32].as_slice(), frozen!("materialized-live.cbor")].concat();
    let creation = creation_item(Hash::from_bytes([6; 32]), 7);
    let handle = handle_item(&[9; 32], &[10; 32], &[11; 32]);
    let mut value = Lt::new();
    check_lt(
        &value,
        "empty",
        (frozen!("lt-empty.bin"), frozen!("lt-empty-digest.bin")),
    );
    value.add(ROW_LABEL, &row);
    check_lt(
        &value,
        "row",
        (frozen!("lt-row.bin"), frozen!("lt-row-digest.bin")),
    );
    value.add(ROW_LABEL, &row);
    check_lt(
        &value,
        "twice",
        (frozen!("lt-twice.bin"), frozen!("lt-twice-digest.bin")),
    );
    value.remove(ROW_LABEL, &row);
    check_lt(
        &value,
        "row",
        (frozen!("lt-row.bin"), frozen!("lt-row-digest.bin")),
    );
    value.remove(ROW_LABEL, &row);
    check_lt(
        &value,
        "empty",
        (frozen!("lt-empty.bin"), frozen!("lt-empty-digest.bin")),
    );
    value.add(ROW_LABEL, &row);
    value.add(CREATION_LABEL, &creation);
    value.add(HANDLE_LABEL, &handle);
    check_lt(
        &value,
        "mixed",
        (frozen!("lt-mixed.bin"), frozen!("lt-mixed-digest.bin")),
    );
    let mut opposite = Lt::new();
    opposite.add(HANDLE_LABEL, &handle);
    opposite.add(CREATION_LABEL, &creation);
    opposite.add(ROW_LABEL, &row);
    assert!(opposite == value);
    let mut left = Lt::new();
    left.add(ROW_LABEL, &row);
    let mut right = Lt::new();
    right.add(HANDLE_LABEL, &handle);
    check_lt(
        &right,
        "handle",
        (frozen!("lt-handle.bin"), frozen!("lt-handle-digest.bin")),
    );
    right.add(CREATION_LABEL, &creation);
    left.merge(&right);
    assert!(left == value);
}
/// Rewrites one fixture; `updating` never returns a directory on wasm.
fn rewrite(
    directory: &std::path::Path,
    name: impl AsRef<std::path::Path>,
    bytes: impl AsRef<[u8]>,
) {
    #[cfg(not(target_family = "wasm"))]
    std::fs::write(directory.join(name), bytes).unwrap();
    #[cfg(target_family = "wasm")]
    unreachable!(
        "{} ({} bytes)",
        directory.join(name).display(),
        bytes.as_ref().len()
    );
}
/// The fixture directory when `ROCKY_UPDATE_CHECKPOINT_VECTORS` asks to
/// rewrite the fixtures instead of checking them.
fn updating() -> Option<std::path::PathBuf> {
    #[cfg(not(target_family = "wasm"))]
    if std::env::var_os("ROCKY_UPDATE_CHECKPOINT_VECTORS").is_some() {
        return Some(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("src/replica/checkpoint/backend_vectors"),
        );
    }
    None
}
fn check_sets(
    actual: &Sets,
    name: &str,
    (state, summaries): (&[u8], &[u8]),
    counts: (u64, u64, u64),
) {
    assert_eq!((actual.rows, actual.tombstones, actual.creations), counts);
    if let Some(directory) = updating() {
        let mut both = actual.rows_summary().digest.to_vec();
        both.extend_from_slice(&actual.creations_summary().digest);
        rewrite(&directory, format!("sets-{name}.bin"), actual.to_bytes());
        rewrite(&directory, format!("sets-{name}-summaries.bin"), both);
        return;
    }
    assert_eq!(actual.to_bytes(), state);
    assert!(Sets::from_bytes(state).unwrap() == *actual);
    assert_eq!(actual.rows_summary().count, counts.0);
    assert_eq!(actual.creations_summary().count, counts.2);
    assert_eq!(actual.rows_summary().digest.as_slice(), &summaries[..32]);
    assert_eq!(
        actual.creations_summary().digest.as_slice(),
        &summaries[32..]
    );
}
#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn actual_sets_replace_remove_and_merge_keep_frozen_state() {
    let live = encode(&materialized(false)).unwrap();
    let deleted = encode(&materialized(true)).unwrap();
    if let Some(directory) = updating() {
        rewrite(&directory, "materialized-live.cbor", &live);
        rewrite(&directory, "materialized-deleted.cbor", &deleted);
    } else {
        assert_eq!(live, frozen!("materialized-live.cbor"));
        assert_eq!(deleted, frozen!("materialized-deleted.cbor"));
    }
    let mut sets = Sets::new();
    check_sets(
        &sets,
        "empty",
        (
            frozen!("sets-empty.bin"),
            frozen!("sets-empty-summaries.bin"),
        ),
        (0, 0, 0),
    );
    sets.replace(&[8; 32], None, &live).unwrap();
    sets.create(Hash::from_bytes([6; 32]), 7);
    check_sets(
        &sets,
        "live",
        (frozen!("sets-live.bin"), frozen!("sets-live-summaries.bin")),
        (1, 0, 1),
    );
    sets.replace(&[8; 32], Some(&live), &deleted).unwrap();
    check_sets(
        &sets,
        "deleted",
        (
            frozen!("sets-deleted.bin"),
            frozen!("sets-deleted-summaries.bin"),
        ),
        (1, 1, 1),
    );
    sets.remove(&[8; 32], &deleted).unwrap();
    check_sets(
        &sets,
        "removed",
        (
            frozen!("sets-removed.bin"),
            frozen!("sets-removed-summaries.bin"),
        ),
        (0, 0, 1),
    );
    sets.replace(&[8; 32], None, &live).unwrap();
    let mut delta = Sets::new();
    delta.replace(&[16; 32], None, &deleted).unwrap();
    delta.create(Hash::from_bytes([17; 32]), 8);
    sets.merge_rows(&delta);
    check_sets(
        &sets,
        "merged",
        (
            frozen!("sets-merged.bin"),
            frozen!("sets-merged-summaries.bin"),
        ),
        (2, 1, 1),
    );
    let mut direct = Sets::new();
    direct.replace(&[8; 32], None, &live).unwrap();
    direct.replace(&[16; 32], None, &deleted).unwrap();
    direct.create(Hash::from_bytes([6; 32]), 7);
    assert!(sets == direct);
}
#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn signed_checkpoint_reproduces_and_verifies_the_software_fixture() {
    let checkpoint = signed();
    if let Some(directory) = updating() {
        for (name, bytes) in [
            ("statement.cbor", encode(&checkpoint.statement).unwrap()),
            ("signing.bin", checkpoint.signing_bytes().unwrap()),
            ("signed.cbor", checkpoint.to_bytes().unwrap()),
            ("signature.bin", checkpoint.signature.to_bytes().to_vec()),
            ("checkpoint-id.bin", checkpoint.id().unwrap().to_vec()),
        ] {
            rewrite(&directory, name, bytes);
        }
        return;
    }
    assert_eq!(
        encode(&checkpoint.statement).unwrap(),
        frozen!("statement.cbor")
    );
    assert_eq!(checkpoint.signing_bytes().unwrap(), frozen!("signing.bin"));
    assert_eq!(checkpoint.to_bytes().unwrap(), frozen!("signed.cbor"));
    assert_eq!(
        checkpoint.signature.to_bytes().as_slice(),
        frozen!("signature.bin")
    );
    assert_eq!(
        checkpoint.id().unwrap().as_slice(),
        frozen!("checkpoint-id.bin")
    );
    checkpoint.verify_signature().unwrap();
    let old = SignedCheckpoint::from_bytes(frozen!("signed.cbor")).unwrap();
    old.verify_signature().unwrap();
    assert_eq!(old.to_bytes().unwrap(), frozen!("signed.cbor"));
    let mut tampered = old.clone();
    tampered.statement.rows.digest[0] ^= 1;
    assert!(
        matches!(tampered.verify_signature(), Err(Error::Rejected(message)) if message == "invalid checkpoint signature or frontier")
    );
    let mut tampered = old;
    let mut signature = tampered.signature.to_bytes();
    signature[0] ^= 1;
    tampered.signature = Signature::from_bytes(&signature);
    assert!(
        matches!(tampered.verify_signature(), Err(Error::Rejected(message)) if message == "invalid checkpoint signature or frontier")
    );
}
