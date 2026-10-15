use rocky_core::*;
fn hex(b: &[u8]) -> String {
    b.iter().map(|v| format!("{v:02x}")).collect()
}
#[test]
fn the_genesis_anchor_keeps_its_vector() {
    assert_eq!(
        hex(&genesis(fixtures::SCOPE, [7; 16])),
        "48ead3a3d0babf04aee931f6f43e86c2f38453111304988dd63867dcdf87469a"
    );
}
#[test]
fn request_sequences_carry_issue_time_and_strictly_increase() {
    let now = 1_800_000_000;
    let first = next_request_sequence(7, now).unwrap().get();
    assert_eq!(issued_at(first), now);
    let second = next_request_sequence(first, now).unwrap().get();
    assert_eq!((second, issued_at(second)), (first + 1, now));
    // A clock that moved backwards keeps the sequence increasing.
    let third = next_request_sequence(second, now - 60).unwrap().get();
    assert_eq!((third, issued_at(third)), (second + 1, now));
    assert!(next_request_sequence(0, u64::MAX >> 8).is_err());
}
