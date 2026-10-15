//! Per-tip logs: each tip orders its own entries, its in-sync set is the
//! trusted nodes that follow it, and the domain bounds how many it holds.
use super::coordinated_tests::{Fixture, T};
use super::*;
use crate::coordinated::{Action, Result as LogResult, TIP_LOGS};
use p2panda_core::PrivateKey;

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
    // A node in sync on `main` follows no other tip.
    assert_eq!(join(&mut f, &nodes[0], dev), Err(CoordinationError::Denied));
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
