//! Schedules: a step list that is a function of the seed alone, so a
//! failing seed replays the same steps whatever they did the first time.
use crate::transport::memory::{Draws, LinkFaults};
use rocky_secure::CrashPoint;
use std::{fmt, time::Duration};

/// A schedule's size; liveness gets `rounds` sync rounds after healing.
#[derive(Clone, Copy, Debug)]
pub(super) struct Scenario {
    pub(super) nodes: usize,
    pub(super) steps: usize,
    pub(super) rounds: usize,
}

/// Nodes are indices; node 0 created the domain and node 1 joined it
/// before the first step. A step that cannot apply is skipped.
#[derive(Clone, Debug)]
pub(super) enum Action {
    Write(usize),
    /// A sync pass with every peer the node can reach.
    Sync(usize),
    /// A sync pass of the first node while only the second is reachable.
    SyncPair(usize, usize),
    Faults(usize, usize, LinkFaults),
    Sever(usize, usize),
    Heal(usize, usize),
    NetworkChange,
    Advance(Duration),
    /// Closes the node and opens it again.
    Restart(usize),
    /// Arms the point, runs the step that reaches it, kills the node and
    /// opens it again; without a point it only kills and reopens.
    Crash(usize, Option<CrashPoint>),
    Seal(usize),
    Compact(usize),
    /// Opens a tip at the latest seal and follows a new bookmark there.
    Branch(usize, String),
    /// Follows a bookmark; `main` leaves a branch.
    Follow(usize, String),
    /// Node 0 creates a partition of the partitioned table.
    Partition(String),
    /// Node 0 shares a partition's key with the node.
    Share(String, usize),
    /// A row in a partition, which only a node holding its key writes.
    WritePart(usize, String),
    /// The first node invites the second, which joins.
    Invite(usize, usize),
    /// Node 0 revokes the node.
    Revoke(usize),
}
impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = |node: &usize| node_name(*node);
        match self {
            Self::Write(node) => write!(f, "write {}", name(node)),
            Self::Sync(node) => write!(f, "sync {}", name(node)),
            Self::SyncPair(node, peer) => write!(f, "sync {} with {}", name(node), name(peer)),
            Self::Faults(a, b, faults) => write!(f, "faults {}-{} {faults:?}", name(a), name(b)),
            Self::Sever(a, b) => write!(f, "sever {}-{}", name(a), name(b)),
            Self::Heal(a, b) => write!(f, "heal {}-{}", name(a), name(b)),
            Self::NetworkChange => write!(f, "network change"),
            Self::Advance(by) => write!(f, "advance {by:?}"),
            Self::Restart(node) => write!(f, "restart {}", name(node)),
            Self::Crash(node, point) => write!(f, "crash {} at {point:?}", name(node)),
            Self::Seal(node) => write!(f, "seal {}", name(node)),
            Self::Compact(node) => write!(f, "compact {}", name(node)),
            Self::Branch(node, bookmark) => write!(f, "branch {} {bookmark}", name(node)),
            Self::Follow(node, bookmark) => write!(f, "follow {} {bookmark}", name(node)),
            Self::Partition(list) => write!(f, "partition {list}"),
            Self::Share(list, node) => write!(f, "share {list} with {}", name(node)),
            Self::WritePart(node, list) => write!(f, "write {} in {list}", name(node)),
            Self::Invite(by, node) => write!(f, "invite {} by {}", name(node), name(by)),
            Self::Revoke(node) => write!(f, "revoke {}", name(node)),
        }
    }
}
pub(super) fn node_name(node: usize) -> String {
    char::from(b'a' + node as u8).to_string()
}

/// Weights of the draw; the device testing's paths are favored by the
/// follow-ups in `plan`.
const WEIGHTS: [(Kind, u64); 19] = [
    (Kind::Write, 20),
    (Kind::Sync, 14),
    (Kind::SyncPair, 8),
    (Kind::Faults, 5),
    (Kind::Sever, 4),
    (Kind::Heal, 4),
    (Kind::NetworkChange, 1),
    (Kind::Advance, 4),
    (Kind::Restart, 3),
    (Kind::Crash, 5),
    (Kind::Seal, 6),
    (Kind::Compact, 4),
    (Kind::Branch, 3),
    (Kind::Follow, 5),
    (Kind::Partition, 3),
    (Kind::Share, 3),
    (Kind::WritePart, 4),
    (Kind::Invite, 4),
    (Kind::Revoke, 1),
];
#[derive(Clone, Copy)]
enum Kind {
    Write,
    Sync,
    SyncPair,
    Faults,
    Sever,
    Heal,
    NetworkChange,
    Advance,
    Restart,
    Crash,
    Seal,
    Compact,
    Branch,
    Follow,
    Partition,
    Share,
    WritePart,
    Invite,
    Revoke,
}

/// Crash points the simulation reaches; eviction needs a partial replica.
const POINTS: [CrashPoint; 7] = [
    CrashPoint::Import,
    CrashPoint::Switch,
    CrashPoint::Prune,
    CrashPoint::Compact,
    CrashPoint::Settle,
    CrashPoint::GroupBeforeCommit,
    CrashPoint::GroupAfterCommit,
];

pub(super) fn plan(seed: u64, scenario: Scenario) -> Vec<Action> {
    let mut draws = Draws::new(seed);
    let total: u64 = WEIGHTS.iter().map(|(_, weight)| weight).sum();
    let mut steps = Vec::with_capacity(scenario.steps);
    // Planned members; the schedule never depends on what happened.
    let mut members = 2;
    let mut revoked = false;
    let mut branches: Vec<String> = Vec::new();
    let mut lists: Vec<String> = Vec::new();
    while steps.len() < scenario.steps {
        let index = steps.len();
        let node = |draws: &mut Draws| draws.below(members as u64) as usize;
        // Only the owner signs tip statements; others are mostly refused.
        let signer = |draws: &mut Draws| match draws.chance(700) {
            true => 0,
            false => draws.below(members as u64) as usize,
        };
        let pair = |draws: &mut Draws| {
            let a = draws.below(scenario.nodes as u64) as usize;
            let b = (a + 1 + draws.below(scenario.nodes as u64 - 1) as usize) % scenario.nodes;
            (a, b)
        };
        let mut pick = draws.below(total);
        let kind = WEIGHTS
            .iter()
            .find(|(_, weight)| {
                let found = pick < *weight;
                pick = pick.saturating_sub(*weight);
                found
            })
            .map_or(Kind::Write, |(kind, _)| *kind);
        let action = match kind {
            Kind::Write => Action::Write(node(&mut draws)),
            Kind::Sync => Action::Sync(node(&mut draws)),
            Kind::SyncPair => {
                let (a, b) = pair(&mut draws);
                Action::SyncPair(a, b)
            }
            Kind::Faults => {
                let (a, b) = pair(&mut draws);
                Action::Faults(a, b, faults(&mut draws))
            }
            Kind::Sever => {
                let (a, b) = pair(&mut draws);
                Action::Sever(a, b)
            }
            Kind::Heal => {
                let (a, b) = pair(&mut draws);
                Action::Heal(a, b)
            }
            Kind::NetworkChange => Action::NetworkChange,
            Kind::Advance => Action::Advance(Duration::from_millis(
                [10, 500, 5_000, 60_000][draws.below(4) as usize],
            )),
            Kind::Restart => Action::Restart(node(&mut draws)),
            Kind::Crash => {
                let point = draws.below(POINTS.len() as u64 + 1) as usize;
                Action::Crash(node(&mut draws), POINTS.get(point).copied())
            }
            Kind::Seal => Action::Seal(signer(&mut draws)),
            Kind::Compact => Action::Compact(signer(&mut draws)),
            Kind::Branch => {
                let name = format!("dev{index}");
                branches.push(name.clone());
                Action::Branch(signer(&mut draws), name)
            }
            Kind::Follow => {
                let bookmark = match draws.below(2) {
                    0 => branches.last().cloned(),
                    _ => None,
                };
                Action::Follow(node(&mut draws), bookmark.unwrap_or_else(|| "main".into()))
            }
            Kind::Partition => {
                let list = format!("list{index}");
                lists.push(list.clone());
                Action::Partition(list)
            }
            Kind::Share | Kind::WritePart if lists.is_empty() => continue,
            Kind::Share => {
                let list = lists[draws.below(lists.len() as u64) as usize].clone();
                Action::Share(list, 1 + draws.below(members as u64 - 1) as usize)
            }
            Kind::WritePart => {
                let list = lists[draws.below(lists.len() as u64) as usize].clone();
                Action::WritePart(node(&mut draws), list)
            }
            Kind::Invite if members < scenario.nodes => {
                members += 1;
                Action::Invite(draws.below(members as u64 - 1) as usize, members - 1)
            }
            Kind::Revoke if !revoked && members > 2 => {
                revoked = true;
                Action::Revoke(1 + draws.below(members as u64 - 1) as usize)
            }
            Kind::Invite | Kind::Revoke => continue,
        };
        // The device testing's paths: a member joining after a seal, a
        // restart during its bootstrap, a seal while another node follows
        // a branch.
        let follow = match &action {
            Action::Seal(_) if members < scenario.nodes && draws.chance(400) => {
                members += 1;
                Some(Action::Invite(0, members - 1))
            }
            Action::Invite(_, joined) if draws.chance(500) => Some(Action::Crash(
                *joined,
                [None, Some(CrashPoint::Import), Some(CrashPoint::Switch)][draws.below(3) as usize],
            )),
            Action::Branch(node, bookmark) if draws.chance(500) => {
                let other = (node + 1 + draws.below(members as u64 - 1) as usize) % members;
                Some(Action::Follow(other, bookmark.clone()))
            }
            Action::Follow(_, bookmark) if bookmark != "main" && draws.chance(500) => {
                Some(Action::Seal(0))
            }
            _ => None,
        };
        steps.push(action);
        steps.extend(follow);
    }
    steps.truncate(scenario.steps);
    steps
}

fn faults(draws: &mut Draws) -> LinkFaults {
    let millis = |draws: &mut Draws, most: u64| Duration::from_millis(draws.below(most + 1));
    LinkFaults {
        latency: millis(draws, 80),
        jitter: millis(draws, 40),
        bandwidth: draws.chance(300).then(|| 16 << draws.below(6)),
        stalls: draws.below(40) as u16,
        stall: millis(draws, 20_000),
        cuts: draws.below(150) as u16,
        cut_within: 1 + draws.below(64 * 1024) as usize,
        refusals: draws.below(150) as u16,
    }
}
