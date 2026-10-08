//! Bounded admission windows, retirement floors and their outcomes, driven
//! with the controllable clock. See `docs/runtime/coordinator-retention.md`.
use rocky_coordinator::{Limits, MemoryCoordinator, Status};
use rocky_core::*;

const DAY: u64 = 86_400;
const T: u64 = 1_900_000_000;

fn coordinator(limits: Limits) -> MemoryCoordinator {
    let c = MemoryCoordinator::with_limits(limits);
    c.set_time(T).unwrap();
    let invokers = (0..=255u8).map(|n| (NodeId([n; 16]), fixtures::OWNER));
    c.register(
        fixtures::SCOPE,
        fixtures::OWNER,
        [7; 16],
        vec![fixtures::LAPTOP, fixtures::PHONE],
        1,
        1,
    )
    .unwrap();
    c.set_invokers(fixtures::SCOPE, fixtures::OWNER, invokers.collect())
        .unwrap();
    c.acquire(fixtures::SCOPE, fixtures::OWNER, fixtures::LAPTOP)
        .unwrap();
    c
}
fn id(node: NodeId, issued: u64, n: u64) -> RequestId {
    RequestId {
        node,
        sequence: AuthorSequence::new((issued << ISSUE_SHIFT) + n).unwrap(),
    }
}
fn proposal(c: &MemoryCoordinator, request: RequestId, input: u8) -> Confirmation {
    let h = c.head(fixtures::SCOPE).unwrap();
    let mut p = Confirmation {
        caller: fixtures::OWNER,
        scope: fixtures::SCOPE,
        configuration: h.configuration,
        executor: h.executor,
        epoch: h.epoch,
        request,
        request_hash: [input; 32],
        expected_sequence: h.sequence,
        expected_head: h.head,
        schema: 1,
        handler: 1,
        result: [9; 32],
        new_head: [0; 32],
    };
    p.new_head = successor(p.expected_head, h.sequence + 1, p.request_hash, p.result);
    p
}
fn status(c: &MemoryCoordinator, request: RequestId) -> Status {
    c.status(fixtures::SCOPE, request).unwrap()
}

#[test]
fn every_outcome_is_honest_and_old_identities_never_execute_again() {
    let c = coordinator(Limits::default());
    let phone = fixtures::PHONE;
    // Reordered: the later identity confirms first; the gap stays admissible.
    let late = c.confirm(proposal(&c, id(phone, T, 3), 3)).unwrap();
    assert_eq!(status(&c, id(phone, T, 1)), Status::Open);
    let early = c.confirm(proposal(&c, id(phone, T, 1), 1)).unwrap();
    assert_eq!(early.sequence, late.sequence + 1);
    // Lost response: the same proposal returns the retained receipt.
    assert_eq!(
        c.confirm(proposal(&c, id(phone, T, 1), 1)),
        Err(CoordinationError::AlreadyConfirmed(Box::new(early.clone())))
    );
    assert_eq!(
        c.lookup(fixtures::SCOPE, id(phone, T, 1)).unwrap(),
        Some(early.clone())
    );
    // Changed input under a retained identity conflicts.
    assert_eq!(
        c.confirm(proposal(&c, id(phone, T, 3), 4)),
        Err(CoordinationError::RequestConflict)
    );
    // Retire the prefix through 1: identity 1 and the never-confirmed 2 are
    // retired, including their outcome; 3 keeps its response.
    c.retire(fixtures::SCOPE, phone, id(phone, T, 2).sequence.get())
        .unwrap();
    for n in [1, 2] {
        assert_eq!(status(&c, id(phone, T, n)), Status::Retired);
        assert_eq!(
            c.confirm(proposal(&c, id(phone, T, n), n as u8)),
            Err(CoordinationError::Retired)
        );
        assert_eq!(
            c.lookup(fixtures::SCOPE, id(phone, T, n)),
            Err(CoordinationError::Retired)
        );
    }
    assert_eq!(c.verify(&early), Err(CoordinationError::Retired));
    c.verify(&late).unwrap();
    // A floor never moves back.
    assert_eq!(
        c.retire(fixtures::SCOPE, phone, 1).unwrap(),
        id(phone, T, 2).sequence.get()
    );

    // Identity 4 is never confirmed. Within admission it is pending; after
    // admission closes it is authoritatively not confirmed; past the horizon
    // nothing distinguishes it from a retired response.
    let never = id(phone, T, 4);
    assert_eq!(status(&c, never), Status::Open);
    c.set_time(T + DAY + 1).unwrap();
    assert_eq!(status(&c, never), Status::NotConfirmed);
    assert_eq!(
        c.lookup(fixtures::SCOPE, never),
        Err(CoordinationError::Expired)
    );
    assert_eq!(
        c.confirm(proposal(&c, never, 4)),
        Err(CoordinationError::Expired)
    );
    assert_eq!(
        status(&c, id(phone, T, 3)),
        Status::Confirmed(Box::new(late.clone()))
    );
    c.set_time(T + 7 * DAY + 1).unwrap();
    assert_eq!(status(&c, never), Status::Retired);
    assert_eq!(status(&c, id(phone, T, 3)), Status::Retired);
    assert_eq!(c.retention().unwrap().sessions, 0);
    // An identity issued ahead of authority time waits.
    let ahead = id(phone, T + 8 * DAY, 1);
    assert_eq!(
        c.confirm(proposal(&c, ahead, 5)),
        Err(CoordinationError::Backpressure)
    );
    assert_eq!(status(&c, ahead), Status::Open);
}

#[test]
fn a_backward_clock_never_reopens_admission_or_restores_responses() {
    let c = coordinator(Limits::default());
    let phone = fixtures::PHONE;
    let receipt = c.confirm(proposal(&c, id(phone, T, 1), 1)).unwrap();
    // Authority time advances when an operation observes the clock.
    c.set_time(T + 8 * DAY).unwrap();
    assert_eq!(c.time().unwrap(), T + 8 * DAY);
    c.set_time(T).unwrap();
    assert_eq!(c.time().unwrap(), T + 8 * DAY);
    assert_eq!(status(&c, id(phone, T, 1)), Status::Retired);
    assert_eq!(
        c.confirm(proposal(&c, id(phone, T, 2), 2)),
        Err(CoordinationError::Retired)
    );
    assert_eq!(c.verify(&receipt), Err(CoordinationError::Retired));
}

#[test]
fn full_windows_backpressure_until_acknowledged_or_expired() {
    let limits = Limits::default();
    let c = coordinator(limits);
    let phone = fixtures::PHONE;
    let mut receipts = Vec::new();
    for n in 1..=limits.window_entries as u64 {
        receipts.push(c.confirm(proposal(&c, id(phone, T, n), n as u8)).unwrap());
    }
    let next = id(phone, T, 100);
    assert_eq!(
        c.confirm(proposal(&c, next, 100)),
        Err(CoordinationError::Backpressure)
    );
    // Nothing was evicted to make room, and retries still answer.
    for receipt in &receipts {
        c.verify(receipt).unwrap();
    }
    // Another session is independent of the full one.
    c.confirm(proposal(&c, id(NodeId([7; 16]), T, 1), 7))
        .unwrap();
    // The invoker's durable acknowledgment frees the window.
    c.retire(fixtures::SCOPE, phone, id(phone, T, 16).sequence.get())
        .unwrap();
    c.confirm(proposal(&c, next, 100)).unwrap();
    // Without acknowledgment, the horizon frees it.
    let other = NodeId([8; 16]);
    for n in 1..=limits.window_entries as u64 {
        c.confirm(proposal(&c, id(other, T, n), 200)).unwrap();
    }
    let blocked = id(other, T + 7 * DAY, 1);
    c.set_time(T + 7 * DAY).unwrap();
    assert_eq!(
        c.confirm(proposal(&c, blocked, 201)),
        Err(CoordinationError::Backpressure)
    );
    c.set_time(T + 7 * DAY + 1).unwrap();
    c.confirm(proposal(&c, blocked, 201)).unwrap();
}

#[test]
fn device_churn_leaves_no_lifetime_ledger() {
    let limits = Limits {
        sessions: 16,
        ..Limits::default()
    };
    let c = coordinator(limits);
    let mut peak = rocky_coordinator::Retention::default();
    let mut refused = 0;
    // Every day sixteen new devices each confirm once and never return.
    for day in 0..80u64 {
        let now = T + day * DAY;
        c.set_time(now).unwrap();
        for n in 0..16u8 {
            let device = NodeId([(day as u8).wrapping_mul(16).wrapping_add(n); 16]);
            match c.confirm(proposal(&c, id(device, now, 1), n)) {
                Ok(_) => {}
                Err(CoordinationError::Backpressure) => refused += 1,
                Err(error) => panic!("{error:?}"),
            }
        }
        let retention = c.retention().unwrap();
        assert!(retention.sessions <= limits.sessions);
        peak.sessions = peak.sessions.max(retention.sessions);
        peak.bytes = peak.bytes.max(retention.bytes);
    }
    // Sessions are bounded by the limit, not by the 1,280 devices seen.
    assert_eq!(peak.sessions, 16);
    assert!(refused > 0);
    c.set_time(T + 90 * DAY).unwrap();
    assert_eq!(c.retention().unwrap().sessions, 0);
}

#[test]
fn retiring_a_never_confirmed_prefix_preserves_replay_protection() {
    let c = coordinator(Limits::default());
    let request = id(fixtures::PHONE, T, 2);
    c.retire(fixtures::SCOPE, request.node, request.sequence.get())
        .unwrap();
    for n in 1..=2 {
        let request = id(fixtures::PHONE, T, n);
        assert_eq!(status(&c, request), Status::Retired);
        assert_eq!(
            c.confirm(proposal(&c, request, 1)),
            Err(CoordinationError::Retired)
        );
    }
    c.confirm(proposal(&c, id(fixtures::PHONE, T, 3), 1))
        .unwrap();
}
