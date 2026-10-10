use rocky_coordinator::MemoryCoordinator;
use rocky_core::*;
fn coordinator() -> MemoryCoordinator {
    let c = MemoryCoordinator::default();
    c.register(
        fixtures::SCOPE,
        fixtures::OWNER,
        [7; 16],
        vec![fixtures::LAPTOP, fixtures::PHONE],
        1,
        1,
    )
    .unwrap();
    c.acquire(fixtures::SCOPE, fixtures::OWNER, fixtures::LAPTOP)
        .unwrap();
    c
}
fn proposal(c: &MemoryCoordinator, n: u64) -> Confirmation {
    let h = c.head(fixtures::SCOPE).unwrap();
    let mut p = Confirmation {
        caller: fixtures::OWNER,
        scope: fixtures::SCOPE,
        configuration: h.configuration,
        executor: h.executor,
        epoch: h.epoch,
        request: RequestId {
            node: fixtures::PHONE,
            sequence: AuthorSequence::new((c.time().unwrap() << ISSUE_SHIFT) + n).unwrap(),
        },
        request_hash: [n as u8; 32],
        expected_sequence: h.sequence,
        expected_head: h.head,
        schema: 1,
        handler: 1,
        result: [9; 32],
        new_head: [0; 32],
    };
    p.new_head = successor(
        p.expected_head,
        p.expected_sequence + 1,
        p.request_hash,
        p.result,
    );
    p
}
#[test]
fn fencing_deduplication_and_competing_heads() {
    let c = coordinator();
    let first = proposal(&c, 1);
    let competing = proposal(&c, 2);
    let receipt = c.confirm(first.clone()).unwrap();
    assert_eq!(c.confirm(competing), Err(CoordinationError::StaleHead));
    let stale = proposal(&c, 3);
    let h = c
        .takeover(fixtures::SCOPE, fixtures::OWNER, fixtures::PHONE, 1)
        .unwrap();
    assert_eq!(h.epoch, 2);
    assert_eq!(c.confirm(stale), Err(CoordinationError::StaleEpoch));
    assert_eq!(c.confirm(first.clone()).unwrap(), receipt);
    c.verify(&receipt).unwrap();
    let mut conflict = first.clone();
    conflict.request_hash = [0; 32];
    assert_eq!(c.confirm(conflict), Err(CoordinationError::RequestConflict));
    let mut conflict = first;
    conflict.result = [0; 32];
    assert!(matches!(
        c.confirm(conflict),
        Err(CoordinationError::AlreadyConfirmed(_))
    ));
    assert_eq!(c.head(fixtures::SCOPE).unwrap().sequence, 1);
}
#[test]
fn denied_proposals_do_not_change_metadata() {
    let c = coordinator();
    let before = c.head(fixtures::SCOPE).unwrap();
    for field in 0..5 {
        let mut p = proposal(&c, 1);
        match field {
            0 => p.scope.domain = DomainId([0; 16]),
            1 => p.executor = NodeId([0; 16]),
            2 => p.configuration = [0; 16],
            3 => p.schema = 2,
            _ => p.new_head = [0; 32],
        };
        assert!(c.confirm(p).is_err());
        assert_eq!(c.head(fixtures::SCOPE).unwrap(), before);
    }
    assert!(
        c.takeover(fixtures::SCOPE, PrincipalId([0; 16]), fixtures::PHONE, 1)
            .is_err()
    );
    assert!(
        c.lookup(
            fixtures::SCOPE,
            RequestId {
                node: fixtures::PHONE,
                sequence: AuthorSequence::new((c.time().unwrap() << ISSUE_SHIFT) + 1).unwrap()
            }
        )
        .unwrap()
        .is_none()
    );
}
#[test]
fn lost_reply_is_reconciled_without_another_sequence() {
    let c = coordinator();
    let p = proposal(&c, 1);
    c.drop_next_reply().unwrap();
    assert_eq!(c.confirm(p.clone()), Err(CoordinationError::Unavailable));
    let old = c.lookup(p.scope, p.request).unwrap().unwrap();
    c.takeover(fixtures::SCOPE, fixtures::OWNER, fixtures::PHONE, 1)
        .unwrap();
    let next = proposal(&c, 2);
    c.confirm(next).unwrap();
    assert_eq!(c.confirm(p).unwrap(), old);
    assert_eq!(c.head(fixtures::SCOPE).unwrap().sequence, 2);
}

#[test]
fn concurrent_proposals_accept_exactly_one_successor() {
    let c = std::sync::Arc::new(coordinator());
    let proposals = [proposal(&c, 1), proposal(&c, 2)];
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let tasks = proposals
        .into_iter()
        .map(|p| {
            let c = c.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                c.confirm(p)
            })
        })
        .collect::<Vec<_>>();
    let results = tasks
        .into_iter()
        .map(|t| t.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Err(CoordinationError::StaleHead)))
            .count(),
        1
    );
    assert_eq!(c.head(fixtures::SCOPE).unwrap().sequence, 1);
}

#[test]
fn explicit_replacement_preserves_lost_head_audit_and_permanently_closes_execution() {
    use rocky_coordinator::ReplacementRecord;
    let c = coordinator();
    let receipt = c.confirm(proposal(&c, 1)).unwrap();
    let lost = c.head(fixtures::SCOPE).unwrap();
    let next = Scope {
        domain: DomainId([98; 16]),
        ..fixtures::SCOPE
    };
    let mut record = ReplacementRecord {
        lost_head: lost.clone(),
        replacement: next,
        acknowledged_loss: false,
        uncertain_effects: true,
    };
    assert_eq!(
        c.replace_domain(fixtures::OWNER, record.clone(), vec![fixtures::PHONE]),
        Err(CoordinationError::Denied)
    );
    record.acknowledged_loss = true;
    c.replace_domain(fixtures::OWNER, record.clone(), vec![fixtures::PHONE])
        .unwrap();
    assert_eq!(
        c.replace_domain(fixtures::OWNER, record.clone(), vec![fixtures::PHONE])
            .unwrap(),
        record
    );
    assert_eq!(c.replacement(fixtures::SCOPE).unwrap(), Some(record));
    let closed = c.head(fixtures::SCOPE).unwrap();
    assert_eq!(closed.sequence, lost.sequence);
    assert_eq!(closed.head, lost.head);
    assert!(closed.epoch > lost.epoch);
    assert_eq!(closed.executor, NodeId([0; 16]));
    c.verify(&receipt).unwrap();
    assert_eq!(
        c.acquire(fixtures::SCOPE, fixtures::OWNER, fixtures::PHONE),
        Err(CoordinationError::Denied)
    );
    assert_eq!(
        c.takeover(
            fixtures::SCOPE,
            fixtures::OWNER,
            fixtures::PHONE,
            closed.epoch
        ),
        Err(CoordinationError::Denied)
    );
    assert_eq!(c.confirm(proposal(&c, 2)), Err(CoordinationError::Denied));
    let replacement = c.acquire(next, fixtures::OWNER, fixtures::PHONE).unwrap();
    assert_eq!(replacement.sequence, 0);
    assert_ne!(replacement.head, lost.head);
}
