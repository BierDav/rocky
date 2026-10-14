//! Owner-signed cut admission on in-memory coordinator state.
use super::coordinated_tests::{Fixture, T};
use super::*;
use crate::coordinated::{Action, HASH_WINDOW, Log, Result as LogResult};
use p2panda_core::PrivateKey;
use rocky_core::coordinated_chain;
use rocky_credentials::{CutPoint, CutStatement, SignedCut};

fn point(position: u64, running: Commitment) -> CutPoint {
    CutPoint { position, running }
}
/// The running hash after `position`, from the genesis anchor.
fn running(head: &crate::coordinated::Head, position: u64) -> Commitment {
    assert!(head.commit as usize == head.hashes.len());
    (1..=position).fold(head.genesis, |running, p| {
        coordinated_chain(running, p, head.hashes[p as usize - 1])
    })
}
fn cut(record: &SignedCut) -> Action {
    Action::Cut {
        record: record.to_bytes().unwrap(),
        authority: Vec::new(),
    }
}
impl Fixture {
    fn commit(&mut self, sequence: u64, hash: u8) {
        let pending = self.append(0, sequence, hash);
        self.resolve(&pending, &[0, 1], T + 2).unwrap();
    }
    fn statement(&self, verified: u64, retained: u64) -> CutStatement {
        let head = self.state.log_head(self.scope).unwrap();
        CutStatement {
            log: self.scope,
            sequence: 1,
            previous: None,
            verified: point(verified, running(&head, verified)),
            retained: point(retained, running(&head, retained)),
            replaced: point(head.commit, head.running_hash),
            epoch: head.epoch,
            issued: T,
        }
    }
}

#[test]
fn an_owner_signed_cut_commits_after_the_exact_head_and_retries_identically() {
    let mut f = Fixture::new(1, Limits::default());
    for n in 1..=3 {
        f.commit(n + 1, n as u8);
    }
    let before = f.state.log_head(f.scope).unwrap();
    let statement = f.statement(1, 2);
    let by_node = SignedCut::sign(statement.clone(), &f.nodes[0]).unwrap();
    assert_eq!(
        f.run(0, cut(&by_node), T + 3),
        Err(CoordinationError::Denied)
    );
    let record = SignedCut::sign(statement.clone(), &f.owner).unwrap();
    let digest = record.digest().unwrap();
    let admitted = f.run(0, cut(&record), T + 3).unwrap();
    let expected = coordinated_chain(before.running_hash, 4, digest);
    assert_eq!(
        admitted,
        Outcome::Coordinated(LogResult::Cut {
            position: 4,
            running_hash: expected,
        })
    );
    let head = f.state.log_head(f.scope).unwrap();
    assert_eq!((head.commit, head.head), (4, 4));
    assert_eq!(head.epoch, before.epoch + 1);
    assert!(head.members.is_empty());
    assert_eq!(head.hashes.back(), Some(&digest));
    let retained = head.cut.as_ref().unwrap();
    assert_eq!((retained.sequence, retained.digest), (1, digest));
    assert_eq!(retained.record, record.to_bytes().unwrap());
    assert_eq!(head.port().cut.unwrap().record, record.to_bytes().unwrap());
    // The same record from another submitter, or the owner, answers alike.
    assert_eq!(f.run(1, cut(&record), T + 4).unwrap(), admitted);
    assert_eq!(f.run_owner(cut(&record), T + 4).unwrap(), admitted);
    // Any other record of the old head is stale.
    let other = SignedCut::sign(
        CutStatement {
            issued: T + 1,
            ..statement
        },
        &f.owner,
    )
    .unwrap();
    assert_eq!(
        f.run(0, cut(&other), T + 4),
        Err(CoordinationError::StaleHead)
    );
    assert_eq!(f.state.log_head(f.scope).unwrap(), head);
    // Nothing appends until a node joins the cut head; then at a+2.
    let append = Action::Append {
        expected: 4,
        epoch: head.epoch,
        hash: [9; 32],
        deadline: T + 10,
        sequence: 9,
    };
    assert_eq!(f.run(0, append, T + 4), Err(CoordinationError::Denied));
    f.run(
        1,
        Action::Join {
            position: 4,
            running_hash: expected,
            sequence: 10,
        },
        T + 4,
    )
    .unwrap();
    let pending = f.append(1, 11, 9);
    assert_eq!(pending.position, 5);
}

#[test]
fn cut_admission_refuses_unchained_points_stale_heads_and_abandoned_cuts() {
    let mut f = Fixture::new(1, Limits::default());
    for n in 1..=3 {
        f.commit(n + 1, n as u8);
    }
    let mut wrong_verified = f.statement(1, 2);
    wrong_verified.verified.running = [1; 32];
    let mut wrong_retained = f.statement(1, 2);
    wrong_retained.retained.running = [1; 32];
    let mut wrong_epoch = f.statement(1, 2);
    wrong_epoch.epoch += 1;
    let mut second = f.statement(1, 2);
    second.sequence = 2;
    second.previous = Some([1; 32]);
    for (statement, error) in [
        (wrong_verified, CoordinationError::Denied),
        (wrong_retained, CoordinationError::Denied),
        (wrong_epoch, CoordinationError::StaleHead),
        (second, CoordinationError::StaleHead),
    ] {
        let record = SignedCut::sign(statement, &f.owner).unwrap();
        assert_eq!(f.run(0, cut(&record), T + 3), Err(error));
    }
    // A pending entry fences the head.
    let pending = f.append(0, 5, 4);
    let mut during = f.statement(3, 3);
    during.replaced.position = pending.position - 1;
    let record = SignedCut::sign(during, &f.owner).unwrap();
    assert_eq!(
        f.run(0, cut(&record), T + 3),
        Err(CoordinationError::StaleHead)
    );
    f.resolve(&pending, &[0, 1], T + 3).unwrap();
    let first = SignedCut::sign(f.statement(0, 2), &f.owner).unwrap();
    f.run(0, cut(&first), T + 4).unwrap();
    let running_hash = f.state.log_head(f.scope).unwrap().running_hash;
    for (node, sequence) in [(0, 6), (1, 2)] {
        let join = Action::Join {
            position: 5,
            running_hash,
            sequence,
        };
        f.run(node, join, T + 4).unwrap();
    }
    f.commit(7, 6);
    let mut next = f.statement(5, 5);
    next.sequence = 2;
    next.previous = Some(first.digest().unwrap());
    // The second cut may not abandon the first one's entry.
    let mut abandoning = next.clone();
    abandoning.verified = f.statement(3, 3).verified;
    abandoning.retained = abandoning.verified;
    let record = SignedCut::sign(abandoning, &f.owner).unwrap();
    assert_eq!(
        f.run(0, cut(&record), T + 5),
        Err(CoordinationError::Denied)
    );
    let record = SignedCut::sign(next, &f.owner).unwrap();
    assert!(matches!(
        f.run(0, cut(&record), T + 5),
        Ok(Outcome::Coordinated(LogResult::Cut { position: 7, .. }))
    ));
    let head = f.state.log_head(f.scope).unwrap();
    assert_eq!(head.cut.unwrap().sequence, 2);
}

#[test]
fn positions_before_the_window_are_endorsed_only_from_the_genesis_anchor() {
    let scope = Scope {
        mesh: MeshId([1; 16]),
        app: AppId([2; 16]),
        domain: DomainId([3; 16]),
    };
    let mut log = Log::new(scope, [4; 16]);
    let commit = HASH_WINDOW as u64 + 10;
    let mut runnings = vec![log.head.genesis];
    for position in 1..=commit {
        let hash = rocky_core::commitment("test", &position.to_be_bytes());
        runnings.push(coordinated_chain(
            runnings[position as usize - 1],
            position,
            hash,
        ));
        log.head.hashes.push_back(hash);
        if log.head.hashes.len() > HASH_WINDOW {
            log.head.hashes.pop_front();
        }
    }
    log.head.commit = commit;
    log.head.head = commit;
    log.head.running_hash = runnings[commit as usize];
    let owner = PrivateKey::new();
    let statement = |verified: u64, retained: u64| CutStatement {
        log: scope,
        sequence: 1,
        previous: None,
        verified: point(verified, runnings[verified as usize]),
        retained: point(retained, runnings[retained as usize]),
        replaced: point(commit, runnings[commit as usize]),
        epoch: 0,
        issued: T,
    };
    let admit = |log: &mut Log, statement| {
        let record = SignedCut::sign(statement, &owner).unwrap();
        log.cut(scope, &record.to_bytes().unwrap(), &[])
    };
    // Before the window, only the genesis anchor is a verified point.
    let mut probe = log.clone();
    assert_eq!(
        admit(&mut probe, statement(5, 6)),
        Err(CoordinationError::Denied)
    );
    let oversized = SignedCut::sign(statement(0, 5), &owner).unwrap();
    assert_eq!(
        probe.cut(
            scope,
            &oversized.to_bytes().unwrap(),
            &vec![0; 8 * 1024 + 1]
        ),
        Err(CoordinationError::Denied)
    );
    assert!(matches!(
        admit(&mut probe, statement(0, 5)),
        Ok(LogResult::Cut { .. })
    ));
    // Inside the window the verified point must chain to the head.
    let mut probe = log.clone();
    let mut tampered = statement(20, 30);
    tampered.verified.running = [1; 32];
    assert_eq!(admit(&mut probe, tampered), Err(CoordinationError::Denied));
    assert!(matches!(
        admit(&mut probe, statement(20, 30)),
        Ok(LogResult::Cut { .. })
    ));
    assert_eq!(probe.head.hashes.len(), HASH_WINDOW);
}
