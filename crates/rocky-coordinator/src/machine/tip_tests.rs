//! Per-tip logs: each tip orders its own entries, its in-sync set is the
//! trusted nodes that follow it, and the domain bounds how many it holds.
use super::coordinated_tests::{Fixture, T};
use super::*;
use crate::coordinated::{Action, ENDED_LOGS, Result as LogResult, TIP_LOGS};
use p2panda_core::PrivateKey;
use rocky_core::LogTerminal;

fn dev() -> TipId {
    TipId([7; 32])
}
impl Fixture {
    fn on(&self, key: &PrivateKey, log: TipLog, action: Action, time: u64) -> Request {
        crate::machine::self_certified(
            self.state.genesis.id().unwrap(),
            crate::transport::nonce(),
            Operation::Coordinated { log, action },
            key,
            time,
        )
        .unwrap()
    }
    fn apply_on(
        &mut self,
        key: &PrivateKey,
        log: TipLog,
        action: Action,
    ) -> Result<Outcome, CoordinationError> {
        let request = self.on(key, log, action, T);
        self.state.apply_at(T, &request)
    }
    /// Trusts a third node, which follows no tip yet.
    fn trust(&mut self) -> PrivateKey {
        let node = PrivateKey::new();
        let mut control = self.state.controls[0].control.clone();
        control.revision += 1;
        control.trusted.push(node.public_key());
        let request = crate::machine::self_certified(
            self.state.genesis.id().unwrap(),
            crate::transport::nonce(),
            Operation::Control(ControlCertificate::sign(control, &self.owner).unwrap()),
            &self.owner,
            T,
        )
        .unwrap();
        self.state.apply_at(T, &request).unwrap();
        node
    }
}
fn join(f: &mut Fixture, key: &PrivateKey, log: TipLog) -> Result<Outcome, CoordinationError> {
    let head = f.state.log_head(log).unwrap();
    f.apply_on(
        key,
        log,
        Action::Join {
            position: head.commit,
            running_hash: head.running_hash,
            sequence: 1,
        },
    )
}

#[test]
fn a_tips_writes_commit_through_its_own_log_and_followers() {
    let mut f = Fixture::new(2, Limits::default());
    let main = tip_log(f.scope);
    let dev = TipLog {
        scope: f.scope,
        tip: dev(),
    };
    let follower = f.trust();
    // The opening names its minimum; a trusted node relays it before a join.
    let nodes = f.nodes.clone();
    assert_eq!(
        f.apply_on(&nodes[0], dev, Action::Configure { minimum: 1 }),
        Ok(Outcome::Coordinated(LogResult::Configured))
    );
    join(&mut f, &follower, dev).unwrap();
    assert_eq!(
        f.apply_on(&follower, dev, Action::Configure { minimum: 2 }),
        Err(CoordinationError::Configuration)
    );
    let head = f.state.log_head(dev).unwrap();
    assert_ne!(head.genesis, f.state.log_head(main).unwrap().genesis);
    let Ok(Outcome::Coordinated(LogResult::Assigned(pending))) = f.apply_on(
        &follower,
        dev,
        Action::Append {
            expected: 0,
            epoch: head.epoch,
            hash: [8; 32],
            deadline: T + 10,
            sequence: 2,
        },
    ) else {
        panic!("assignment")
    };
    let ack = f.on(
        &follower,
        dev,
        Action::Acknowledge {
            pending: (*pending).clone(),
        },
        T + 1,
    );
    let resolve = f.on(
        &f.voter.clone(),
        dev,
        Action::Resolve {
            pending: *pending,
            acknowledgments: vec![ack],
        },
        T + 2,
    );
    assert!(matches!(
        f.state.apply_at(T + 2, &resolve),
        Ok(Outcome::Coordinated(LogResult::Committed {
            position: 1,
            ..
        }))
    ));
    // `main`'s in-sync set saw nothing of it.
    let head = f.state.log_head(main).unwrap();
    assert_eq!(
        (head.commit, head.pending, head.members.len()),
        (0, None, 2)
    );
    assert!(head.hashes.is_empty());
    assert_eq!(f.state.log_head(dev).unwrap().minimum, 1);
}

#[test]
fn a_node_joining_another_tip_leaves_the_log_it_was_in_sync_on() {
    let mut f = Fixture::new(1, Limits::default());
    let main = tip_log(f.scope);
    let dev = TipLog {
        scope: f.scope,
        tip: dev(),
    };
    let nodes = f.nodes.clone();
    let before = f.state.log_head(main).unwrap();
    assert_eq!(before.members.len(), 2);
    join(&mut f, &nodes[0], dev).unwrap();
    let head = f.state.log_head(main).unwrap();
    assert_eq!(head.epoch, before.epoch + 1);
    assert!(!head.member(nodes[0].public_key()) && head.member(nodes[1].public_key()));
    assert!(f.state.log_head(dev).unwrap().member(nodes[0].public_key()));
    // A node does not leave a tip while an entry there is pending.
    let Ok(Outcome::Coordinated(LogResult::Assigned(_))) = f.apply_on(
        &nodes[1],
        main,
        Action::Append {
            expected: head.commit,
            epoch: head.epoch,
            hash: [8; 32],
            deadline: T + 10,
            sequence: 9,
        },
    ) else {
        panic!("assignment")
    };
    assert_eq!(
        join(&mut f, &nodes[1], dev),
        Err(CoordinationError::Backpressure)
    );
    assert!(
        f.state
            .log_head(main)
            .unwrap()
            .member(nodes[1].public_key())
    );
}

#[test]
fn a_tip_no_trusted_node_follows_refuses_writes_and_logs_are_bounded() {
    let mut f = Fixture::new(1, Limits::default());
    let nodes = f.nodes.clone();
    let orphan = TipLog {
        scope: f.scope,
        tip: dev(),
    };
    let head = f.state.log_head(orphan).unwrap();
    assert_eq!((head.commit, head.minimum), (0, 1));
    assert_eq!(
        f.apply_on(
            &nodes[0],
            orphan,
            Action::Append {
                expected: 0,
                epoch: 0,
                hash: [1; 32],
                deadline: T + 10,
                sequence: 2,
            },
        ),
        Err(CoordinationError::Denied)
    );
    // Neither the read nor the refusal opened a log.
    assert!(
        f.state
            .state
            .find(f.scope)
            .unwrap()
            .coordinated
            .get(dev())
            .is_none()
    );
    for byte in 10..9 + TIP_LOGS as u8 {
        let log = TipLog {
            scope: f.scope,
            tip: TipId([byte; 32]),
        };
        f.apply_on(&f.owner.clone(), log, Action::Configure { minimum: 1 })
            .unwrap();
    }
    assert_eq!(
        f.apply_on(&f.owner.clone(), orphan, Action::Configure { minimum: 1 }),
        Err(CoordinationError::Backpressure)
    );
}

fn sealing(digest: u8, continuation: Option<TipId>) -> Action {
    Action::Seal {
        terminal: LogTerminal {
            digest: [digest; 32],
            continuation,
        },
    }
}

#[test]
fn a_terminal_entry_aborts_the_pending_one_and_ends_the_log() {
    let mut f = Fixture::new(1, Limits::default());
    let main = tip_log(f.scope);
    let nodes = f.nodes.clone();
    let first = f.append(0, 2, 1);
    f.resolve(&first, &[0, 1], T + 2).unwrap();
    let pending = f.append(1, 2, 2);
    let committed = f.apply_on(&nodes[0], main, sealing(9, Some(dev())));
    let Ok(Outcome::Coordinated(LogResult::Committed {
        position,
        running_hash,
    })) = committed
    else {
        panic!("{committed:?}")
    };
    // It took the pending entry's position; the sealed state is entry 1.
    assert_eq!(position, pending.position);
    let head = f.state.log_head(main).unwrap();
    assert_eq!(
        (head.commit, head.pending, head.members.len()),
        (2, None, 0)
    );
    assert_eq!(head.hashes.back(), Some(&[9; 32]));
    assert_eq!(
        head.terminal,
        Some(LogTerminal {
            digest: [9; 32],
            continuation: Some(dev()),
        })
    );
    // A retry answers alike; neither another seal nor a move diverges.
    assert_eq!(
        f.apply_on(&nodes[1], main, sealing(9, Some(dev()))),
        Ok(Outcome::Coordinated(LogResult::Committed {
            position,
            running_hash
        }))
    );
    assert_eq!(
        f.apply_on(&nodes[1], main, sealing(8, None)),
        Err(CoordinationError::Sealed)
    );
    // Its acknowledgments no longer resolve anything.
    assert!(f.resolve(&pending, &[0, 1], T + 3).is_err());
    for action in [
        Action::Append {
            expected: 2,
            epoch: head.epoch,
            hash: [3; 32],
            deadline: T + 10,
            sequence: 3,
        },
        Action::Join {
            position: 2,
            running_hash,
            sequence: 4,
        },
    ] {
        assert_eq!(
            f.apply_on(&nodes[0], main, action),
            Err(CoordinationError::Sealed)
        );
    }
    assert_eq!(
        f.apply_on(&f.owner.clone(), main, Action::Configure { minimum: 1 }),
        Err(CoordinationError::Sealed)
    );
    // Its nodes follow the continuation.
    let next = TipLog {
        scope: f.scope,
        tip: dev(),
    };
    join(&mut f, &nodes[0], next).unwrap();
}

#[test]
fn ended_logs_are_retained_up_to_their_bound_and_open_none() {
    let mut f = Fixture::new(1, Limits::default());
    let (owner, scope) = (f.owner.clone(), f.scope);
    let log = |byte: u8| TipLog {
        scope,
        tip: TipId([byte; 32]),
    };
    for byte in 0..=ENDED_LOGS as u8 {
        f.apply_on(&owner, log(byte), sealing(byte, None)).unwrap();
    }
    // The earliest ended log is dropped; the rest still report their end.
    assert_eq!(f.state.log_head(log(0)).unwrap().terminal, None);
    for byte in 1..=ENDED_LOGS as u8 {
        let head = f.state.log_head(log(byte)).unwrap();
        assert_eq!(head.terminal.map(|t| t.digest), Some([byte; 32]));
    }
    // Ended logs leave room for every open one.
    for byte in 100..99 + TIP_LOGS as u8 {
        f.apply_on(&owner, log(byte), Action::Configure { minimum: 1 })
            .unwrap();
    }
}
