//! Byte fixtures captured on the software backend; provenance is in the fixture README.
use super::*;
use rocky_core::{AppId, DomainId, MeshId, ResourceGrant, Scope};
use rocky_credentials::{AuthorRange, Authority, device_chain};

fn materialized(deleted: bool) -> Materialized {
    let node = PrivateKey::from_bytes(&[42; 32]);
    let stamp = Stamp {
        clock: 7,
        author: node.public_key().into(),
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
            AuthorRange::ALL,
            (1_800_000_000, 86400, 0),
        )
        .unwrap();
    let credentials = device_chain(root, &user, node.public_key(), 7).unwrap();
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
                author: 7,
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
fn check_lt(actual: &Lt, state: &[u8], digest: &[u8]) {
    assert_eq!(actual.to_bytes(), state);
    assert_eq!(actual.digest().as_slice(), digest);
    assert!(Lt::from_bytes(state).unwrap() == *actual);
}
#[test]
fn full_lanes_keep_independent_bytes_and_wrapping() {
    let row = [[8; 32].as_slice(), frozen!("materialized-live.cbor")].concat();
    let creation = creation_item(Hash::from_bytes([6; 32]), 7);
    let handle = handle_item(&[9; 32], &[10; 32], &[11; 32]);
    let mut value = Lt::new();
    check_lt(
        &value,
        frozen!("lt-empty.bin"),
        frozen!("lt-empty-digest.bin"),
    );
    value.add(ROW_LABEL, &row);
    check_lt(&value, frozen!("lt-row.bin"), frozen!("lt-row-digest.bin"));
    value.add(ROW_LABEL, &row);
    check_lt(
        &value,
        frozen!("lt-twice.bin"),
        frozen!("lt-twice-digest.bin"),
    );
    value.remove(ROW_LABEL, &row);
    check_lt(&value, frozen!("lt-row.bin"), frozen!("lt-row-digest.bin"));
    value.remove(ROW_LABEL, &row);
    check_lt(
        &value,
        frozen!("lt-empty.bin"),
        frozen!("lt-empty-digest.bin"),
    );
    value.add(ROW_LABEL, &row);
    value.add(CREATION_LABEL, &creation);
    value.add(HANDLE_LABEL, &handle);
    check_lt(
        &value,
        frozen!("lt-mixed.bin"),
        frozen!("lt-mixed-digest.bin"),
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
        frozen!("lt-handle.bin"),
        frozen!("lt-handle-digest.bin"),
    );
    right.add(CREATION_LABEL, &creation);
    left.merge(&right);
    assert!(left == value);
}
fn check_sets(actual: &Sets, state: &[u8], summaries: &[u8], counts: (u64, u64, u64)) {
    assert_eq!((actual.rows, actual.tombstones, actual.creations), counts);
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
#[test]
fn actual_sets_replace_remove_and_merge_keep_frozen_state() {
    let live = encode(&materialized(false)).unwrap();
    let deleted = encode(&materialized(true)).unwrap();
    assert_eq!(live, frozen!("materialized-live.cbor"));
    assert_eq!(deleted, frozen!("materialized-deleted.cbor"));
    let mut sets = Sets::new();
    check_sets(
        &sets,
        frozen!("sets-empty.bin"),
        frozen!("sets-empty-summaries.bin"),
        (0, 0, 0),
    );
    sets.replace(&[8; 32], None, &live).unwrap();
    sets.create(Hash::from_bytes([6; 32]), 7);
    check_sets(
        &sets,
        frozen!("sets-live.bin"),
        frozen!("sets-live-summaries.bin"),
        (1, 0, 1),
    );
    sets.replace(&[8; 32], Some(&live), &deleted).unwrap();
    check_sets(
        &sets,
        frozen!("sets-deleted.bin"),
        frozen!("sets-deleted-summaries.bin"),
        (1, 1, 1),
    );
    sets.remove(&[8; 32], &deleted).unwrap();
    check_sets(
        &sets,
        frozen!("sets-removed.bin"),
        frozen!("sets-removed-summaries.bin"),
        (0, 0, 1),
    );
    sets.replace(&[8; 32], None, &live).unwrap();
    let mut delta = Sets::new();
    delta.replace(&[16; 32], None, &deleted).unwrap();
    delta.create(Hash::from_bytes([17; 32]), 8);
    sets.merge_rows(&delta);
    check_sets(
        &sets,
        frozen!("sets-merged.bin"),
        frozen!("sets-merged-summaries.bin"),
        (2, 1, 1),
    );
    let mut direct = Sets::new();
    direct.replace(&[8; 32], None, &live).unwrap();
    direct.replace(&[16; 32], None, &deleted).unwrap();
    direct.create(Hash::from_bytes([6; 32]), 7);
    assert!(sets == direct);
}
#[test]
fn signed_checkpoint_reproduces_and_verifies_the_software_fixture() {
    let checkpoint = signed();
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
