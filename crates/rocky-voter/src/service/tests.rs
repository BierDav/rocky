use super::*;

#[test]
fn saturated_voter_connections_are_counted_in_status_metrics() {
    let counters = Counters::default();
    let slots = Arc::new(Semaphore::new(VOTER_CONNECTION_SLOTS));
    let held: Vec<_> = (0..VOTER_CONNECTION_SLOTS)
        .map(|_| counters.connection_slot(&slots).unwrap())
        .collect();
    assert!(counters.connection_slot(&slots).is_none());
    assert_eq!(counters.snapshot().busy, 1);
    assert_eq!(counters.snapshot().requests, 0);
    drop(held);
    assert!(counters.connection_slot(&slots).is_some());
    assert_eq!(counters.snapshot().busy, 1);
}
