use rocky_core::{AuthorSequence, NodeId, OperationId, fixtures, state_commitment};
use todo_app::{VisibleRow, decode_snapshot, encode_snapshot};
#[test]
fn state_golden_preserves_exact_utf8_and_rejects_noncanonical_rows() {
    let row = VisibleRow {
        id: OperationId {
            node: fixtures::LAPTOP,
            sequence: AuthorSequence::new(1).unwrap(),
        },
        title: "☃".into(),
        completed: true,
    };
    let bytes = encode_snapshot(std::slice::from_ref(&row)).unwrap();
    let hex = bytes.iter().map(|v| format!("{v:02x}")).collect::<String>();
    assert_eq!(
        hex,
        "0100000055555555555555555555555555555555010000000000000003000000e2988301"
    );
    assert_eq!(decode_snapshot(&bytes).unwrap(), vec![row.clone()]);
    let mut invalid = bytes.clone();
    *invalid.last_mut().unwrap() = 2;
    assert!(decode_snapshot(&invalid).is_err());
    let mut invalid = bytes.clone();
    invalid.push(0);
    assert!(decode_snapshot(&invalid).is_err());
    for n in 0..bytes.len() {
        assert!(decode_snapshot(&bytes[..n]).is_err());
    }
    assert!(encode_snapshot(&[row.clone(), row.clone()]).is_err());
    let earlier = VisibleRow {
        id: OperationId {
            node: NodeId([0; 16]),
            sequence: row.id.sequence,
        },
        ..row.clone()
    };
    assert!(encode_snapshot(&[row, earlier]).is_err());
    let digest = state_commitment(&bytes)
        .iter()
        .map(|v| format!("{v:02x}"))
        .collect::<String>();
    assert_eq!(
        digest,
        "2eeac0e10b0c436bd398a96c5ccead167656c80fc3ce2b8be8c48623ca071962"
    );
}
