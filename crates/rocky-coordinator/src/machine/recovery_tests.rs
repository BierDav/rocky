//! The owner's recovery seal of a stuck log on in-memory coordinator state.
use super::coordinated_tests::{Fixture, T};
use super::*;
use crate::coordinated::{Action, HASH_WINDOW, Log, Result as LogResult};
use p2panda_core::PrivateKey;
use rocky_core::{LogTerminal, coordinated_chain};
use rocky_credentials::{
    AuthorMark, LogPoint, Recovery, SEAL_FORMAT, SealAction, SealStatement, SignedSeal,
};

fn point(position: u64, running: Commitment) -> LogPoint {
    LogPoint { position, running }
}
/// The running hash after `position`, from the genesis anchor.
fn running(head: &crate::coordinated::Head, position: u64) -> Commitment {
    assert!(head.commit as usize == head.hashes.len());
    (1..=position).fold(head.genesis, |running, p| {
        coordinated_chain(running, p, head.hashes[p as usize - 1])
    })
}
fn statement(log: TipLog, recovery: Recovery) -> SealStatement {
    SealStatement {
        format: SEAL_FORMAT,
        scope: log.scope,
        tip: log.tip,
        frontier: vec![AuthorMark {
            author: 1,
            sequence: 4,
        }],
        action: SealAction::Continue { schema: [5; 32] },
        recovery: Some(recovery),
    }
}
fn recover(seal: &SignedSeal) -> Action {
    Action::Recover {
        seal: seal.to_bytes().unwrap(),
    }
}
fn terminal(seal: &SignedSeal) -> LogTerminal {
    LogTerminal {
        digest: seal.id().unwrap(),
        continuation: seal.successor().unwrap(),
    }
}
impl Fixture {
    fn commit(&mut self, sequence: u64, hash: u8) {
        let pending = self.append(0, sequence, hash);
        self.resolve(&pending, &[0, 1], T + 2).unwrap();
    }
    fn recovery(&self, retained: u64) -> Recovery {
        let head = self.state.log_head(tip_log(self.scope)).unwrap();
        Recovery {
            retained: point(retained, running(&head, retained)),
            replaced: point(head.commit, head.running_hash),
        }
    }
}

#[test]
fn the_owners_recovery_ends_the_exact_head_without_the_in_sync_set_and_retries_identically() {
    let mut f = Fixture::new(1, Limits::default());
    for n in 1..=3 {
        f.commit(n + 1, n as u8);
    }
    let log = tip_log(f.scope);
    let before = f.state.log_head(log).unwrap();
    assert_eq!(before.members.len(), 2);
    let by_node = SignedSeal::sign(statement(log, f.recovery(1)), &f.nodes[0]).unwrap();
    assert_eq!(
        f.run(0, recover(&by_node), T + 3),
        Err(CoordinationError::Denied)
    );
    let seal = SignedSeal::sign(statement(log, f.recovery(1)), &f.owner).unwrap();
    let admitted = f.run(0, recover(&seal), T + 3).unwrap();
    let expected = coordinated_chain(before.running_hash, 4, seal.id().unwrap());
    assert_eq!(
        admitted,
        Outcome::Coordinated(LogResult::Committed {
            position: 4,
            running_hash: expected,
        })
    );
    let head = f.state.log_head(log).unwrap();
    assert_eq!((head.commit, head.head), (4, 4));
    assert_eq!(head.epoch, before.epoch + 1);
    assert!(head.members.is_empty());
    assert_eq!(head.terminal, Some(terminal(&seal)));
    // The same seal from another submitter, or the owner, answers alike.
    assert_eq!(f.run(1, recover(&seal), T + 4).unwrap(), admitted);
    assert_eq!(f.run_owner(recover(&seal), T + 4).unwrap(), admitted);
    // Another recovery, an ordinary seal or a write never follows it.
    let other = SignedSeal::sign(
        SealStatement {
            recovery: Some(Recovery {
                retained: before_point(&before, 2),
                replaced: point(before.commit, before.running_hash),
            }),
            ..statement(log, f.recovery(1))
        },
        &f.owner,
    )
    .unwrap();
    assert_eq!(
        f.run(0, recover(&other), T + 4),
        Err(CoordinationError::Sealed)
    );
    let ordinary = Action::Seal {
        terminal: LogTerminal {
            digest: [7; 32],
            continuation: None,
        },
    };
    assert_eq!(f.run(1, ordinary, T + 4), Err(CoordinationError::Sealed));
    assert_eq!(f.state.log_head(log).unwrap(), head);
}
fn before_point(head: &crate::coordinated::Head, position: u64) -> LogPoint {
    point(position, running(head, position))
}

#[test]
fn a_recovery_names_the_exact_head_and_a_prefix_that_chains_to_it() {
    let mut f = Fixture::new(1, Limits::default());
    for n in 1..=3 {
        f.commit(n + 1, n as u8);
    }
    let log = tip_log(f.scope);
    let mut wrong_retained = f.recovery(2);
    wrong_retained.retained.running = [1; 32];
    let mut wrong_genesis = f.recovery(0);
    wrong_genesis.retained.running = [1; 32];
    let mut stale = f.recovery(2);
    stale.replaced = before_point(&f.state.log_head(log).unwrap(), 2);
    let other_tip = TipLog {
        scope: f.scope,
        tip: TipId([8; 32]),
    };
    for (statement, error) in [
        (statement(log, wrong_retained), CoordinationError::Denied),
        (statement(log, wrong_genesis), CoordinationError::Denied),
        (statement(log, stale), CoordinationError::StaleHead),
        (
            statement(other_tip, f.recovery(2)),
            CoordinationError::Denied,
        ),
    ] {
        let seal = SignedSeal::sign(statement, &f.owner).unwrap();
        assert_eq!(f.run(0, recover(&seal), T + 3), Err(error));
    }
    // A pending entry is aborted: the recovery replaces the commit before it.
    let pending = f.append(0, 5, 4);
    let seal = SignedSeal::sign(statement(log, f.recovery(0)), &f.owner).unwrap();
    assert!(matches!(
        f.run(0, recover(&seal), T + 3),
        Ok(Outcome::Coordinated(LogResult::Committed {
            position: 4,
            ..
        }))
    ));
    assert_eq!(f.state.log_head(log).unwrap().pending, None);
    assert!(f.resolve(&pending, &[0, 1], T + 4).is_err());
}

#[test]
fn a_prefix_before_the_window_is_endorsed_by_the_owner_alone() {
    let scope = Scope {
        mesh: MeshId([1; 16]),
        app: AppId([2; 16]),
        domain: DomainId([3; 16]),
    };
    let mut log = Log::new(tip_log(scope), [4; 16], 1);
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
    let recovered = |log: &mut Log, retained: LogPoint| {
        let seal = SignedSeal::sign(
            statement(
                tip_log(scope),
                Recovery {
                    retained,
                    replaced: point(commit, runnings[commit as usize]),
                },
            ),
            &owner,
        )
        .unwrap();
        log.recover(
            tip_log(scope),
            &seal.to_bytes().unwrap(),
            owner.public_key(),
        )
    };
    // Before the window the owner endorses the prefix; genesis is checked.
    let mut probe = log.clone();
    assert_eq!(
        recovered(&mut probe, point(0, [1; 32])),
        Err(CoordinationError::Denied)
    );
    assert!(recovered(&mut probe, point(5, [1; 32])).is_ok());
    // Inside the window the prefix must chain to the head.
    let mut probe = log.clone();
    assert_eq!(
        recovered(&mut probe, point(30, [1; 32])),
        Err(CoordinationError::Denied)
    );
    assert!(recovered(&mut probe, point(30, runnings[30])).is_ok());
    assert_eq!(probe.head.hashes.len(), HASH_WINDOW);
}

#[test]
fn only_a_node_of_the_in_sync_set_ends_a_log_with_an_ordinary_seal() {
    let scope = Scope {
        mesh: MeshId([1; 16]),
        app: AppId([2; 16]),
        domain: DomainId([3; 16]),
    };
    let (member, outsider) = (
        PrivateKey::new().public_key(),
        PrivateKey::new().public_key(),
    );
    let mut log = Log::new(tip_log(scope), [4; 16], 1);
    let terminal = LogTerminal {
        digest: [7; 32],
        continuation: None,
    };
    let join = |log: &Log| Action::Join {
        position: log.head.commit,
        running_hash: log.head.running_hash,
        sequence: 1,
    };
    let append = |log: &Log, sequence| Action::Append {
        expected: log.head.commit,
        epoch: log.head.epoch,
        hash: [1; 32],
        deadline: T + 10,
        sequence,
    };
    // An empty log has nothing to hold: anyone ends it, aborting a pending
    // entry.
    let mut empty = log.clone();
    empty.apply(member, &join(&empty), T, true).unwrap();
    empty.apply(member, &append(&empty, 2), T, true).unwrap();
    assert!(matches!(
        empty.seal(outsider, terminal),
        Ok(LogResult::Committed { position: 1, .. })
    ));
    assert_eq!(empty.head.pending, None);
    log.apply(member, &join(&log), T, true).unwrap();
    let Ok(LogResult::Assigned(pending)) = log.apply(member, &append(&log, 2), T, true) else {
        panic!("no slot")
    };
    log.resolve(&pending, &[member], T).unwrap();
    // Once an entry committed, only a node holding it ends the log.
    let before = log.head.clone();
    assert_eq!(log.seal(outsider, terminal), Err(CoordinationError::Denied));
    assert_eq!(log.head, before);
    // A member ends it while another entry is pending, which aborts.
    let Ok(LogResult::Assigned(_)) = log.apply(member, &append(&log, 3), T, true) else {
        panic!("no slot")
    };
    let ended = log.seal(member, terminal).unwrap();
    assert!(matches!(ended, LogResult::Committed { position: 2, .. }));
    assert_eq!(log.head.pending, None);
    // The set is empty now: the same terminal still answers, another is
    // `Sealed`, from anyone.
    assert_eq!(log.seal(outsider, terminal).unwrap(), ended);
    assert_eq!(
        log.seal(
            member,
            LogTerminal {
                digest: [8; 32],
                continuation: None,
            }
        ),
        Err(CoordinationError::Sealed)
    );
}
