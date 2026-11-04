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
    pub(super) fixture: Fixture,
    pub(super) wall: Wall,
}

/// The members' shared wall clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Wall {
    /// Moves with the task clock and between steps.
    Moving,
    /// Stands still, as a coarse or lagging clock at its worst.
    Frozen,
}

/// The schema a run uses and the writes its schedules draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Fixture {
    /// `items` pairs and `parts` rows, with branches.
    Items,
    /// Boards and cards with keys, checks and an assertion, on `main`.
    Relational,
}
impl Fixture {
    pub(super) fn schema(self) -> &'static str {
        match self {
            Self::Items => super::cluster::SCHEMA,
            Self::Relational => super::relational::SCHEMA,
        }
    }
    /// The tables an invited writer reads.
    pub(super) fn scopes(self) -> [&'static str; 2] {
        match self {
            Self::Items => ["items", "parts"],
            Self::Relational => ["boards", "cards"],
        }
    }
}

/// A relational write; indices pick among the rows the author reads.
#[derive(Clone, Debug)]
pub(super) enum Change {
    Board {
        list: String,
        code: u8,
        cap: u8,
    },
    Card {
        list: String,
        board: u8,
        pinned: Option<u8>,
        name: u8,
        points: u8,
    },
    /// A board and a card on it, in one transaction of two members.
    Pair {
        list: String,
        code: u8,
        name: u8,
        points: u8,
    },
    Raise {
        list: String,
        card: u8,
        by: i8,
    },
    Recode {
        list: String,
        board: u8,
        code: u8,
    },
    DropBoard {
        list: String,
        board: u8,
    },
    DropCard {
        list: String,
        card: u8,
    },
    MoveCard {
        from: String,
        to: String,
        card: u8,
        board: u8,
    },
    MoveBoard {
        from: String,
        to: String,
        board: u8,
    },
}

/// Nodes are indices; node 0 created the domain and node 1 joined it
/// before the first step. A step that cannot apply is skipped.
#[derive(Clone, Debug)]
pub(super) enum Action {
    Write(usize),
    /// A write racing the node's own sync pass: the interactive probe.
    Probe(usize),
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
    /// A relational write.
    Change(usize, Change),
    /// The node becomes partial and evicts the list's partitions.
    Evict(usize, String),
    /// The node holds the list's partitions again.
    Hold(usize, String),
    /// The node's networking goes off or on.
    Offline(usize),
    Online(usize),
    /// A write by a node whose clock runs this far ahead.
    WriteAhead(usize, Duration),
    /// Reports the node's frames waiting for its clock and its item keys.
    Ahead(usize),
}
impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = |node: &usize| node_name(*node);
        match self {
            Self::Write(node) => write!(f, "write {}", name(node)),
            Self::Probe(node) => write!(f, "probe {}", name(node)),
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
            Self::Change(node, change) => write!(f, "change {} {change:?}", name(node)),
            Self::Evict(node, list) => write!(f, "evict {} {list}", name(node)),
            Self::Hold(node, list) => write!(f, "hold {} {list}", name(node)),
            Self::Offline(node) => write!(f, "offline {}", name(node)),
            Self::Online(node) => write!(f, "online {}", name(node)),
            Self::WriteAhead(node, by) => write!(f, "write {} {by:?} ahead", name(node)),
            Self::Ahead(node) => write!(f, "ahead {}", name(node)),
        }
    }
}
pub(super) fn node_name(node: usize) -> String {
    char::from(b'a' + node as u8).to_string()
}

/// Weights of the draw; the device testing's paths are favored by the
/// follow-ups in `plan`.
const WEIGHTS: [(Kind, u64); 20] = [
    (Kind::Write, 20),
    (Kind::Probe, 6),
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
    Probe,
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
    if scenario.fixture == Fixture::Relational {
        return relational(seed, scenario);
    }
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
            Kind::Probe => Action::Probe(node(&mut draws)),
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

/// Lists the relational setup creates and shares with node 1.
pub(super) const LISTS: [&str; 2] = ["l0", "l1"];

/// Weights of the relational draw: writes and the lifecycle around them.
const RELATIONAL: [(Lifecycle, u64); 18] = [
    (Lifecycle::Change, 44),
    (Lifecycle::Sync, 14),
    (Lifecycle::SyncPair, 8),
    (Lifecycle::Sever, 3),
    (Lifecycle::Heal, 3),
    (Lifecycle::Advance, 2),
    (Lifecycle::Restart, 3),
    (Lifecycle::Crash, 5),
    (Lifecycle::Seal, 7),
    (Lifecycle::Compact, 5),
    (Lifecycle::Partition, 2),
    (Lifecycle::Share, 4),
    (Lifecycle::Invite, 4),
    (Lifecycle::Revoke, 1),
    (Lifecycle::Evict, 3),
    (Lifecycle::Hold, 3),
    (Lifecycle::Offline, 3),
    (Lifecycle::Online, 3),
];
#[derive(Clone, Copy)]
enum Lifecycle {
    Change,
    Sync,
    SyncPair,
    Sever,
    Heal,
    Advance,
    Restart,
    Crash,
    Seal,
    Compact,
    Partition,
    Share,
    Invite,
    Revoke,
    Evict,
    Hold,
    Offline,
    Online,
}
/// Every crash point; those a step does not reach report so.
const RELATIONAL_POINTS: [CrashPoint; 12] = CrashPoint::ALL;

fn relational(seed: u64, scenario: Scenario) -> Vec<Action> {
    let mut draws = Draws::new(seed);
    let total: u64 = RELATIONAL.iter().map(|(_, weight)| weight).sum();
    let mut steps = Vec::with_capacity(scenario.steps);
    let mut members = 2;
    let mut revoked = false;
    let mut lists: Vec<String> = LISTS.iter().map(|list| list.to_string()).collect();
    while steps.len() < scenario.steps {
        let index = steps.len();
        let node = |draws: &mut Draws| draws.below(members as u64) as usize;
        let other = |draws: &mut Draws| 1 + draws.below(members as u64 - 1) as usize;
        let signer = |draws: &mut Draws| match draws.chance(800) {
            true => 0,
            false => draws.below(members as u64) as usize,
        };
        let pair = |draws: &mut Draws| {
            let a = draws.below(scenario.nodes as u64) as usize;
            let b = (a + 1 + draws.below(scenario.nodes as u64 - 1) as usize) % scenario.nodes;
            (a, b)
        };
        let mut pick = draws.below(total);
        let kind = RELATIONAL
            .iter()
            .find(|(_, weight)| {
                let found = pick < *weight;
                pick = pick.saturating_sub(*weight);
                found
            })
            .map_or(Lifecycle::Change, |(kind, _)| *kind);
        let list = |draws: &mut Draws| lists[draws.below(lists.len() as u64) as usize].clone();
        let action = match kind {
            Lifecycle::Change => {
                let author = node(&mut draws);
                Action::Change(author, change(&mut draws, &lists))
            }
            Lifecycle::Sync => Action::Sync(node(&mut draws)),
            Lifecycle::SyncPair => {
                let (a, b) = pair(&mut draws);
                Action::SyncPair(a, b)
            }
            Lifecycle::Sever => {
                let (a, b) = pair(&mut draws);
                Action::Sever(a, b)
            }
            Lifecycle::Heal => {
                let (a, b) = pair(&mut draws);
                Action::Heal(a, b)
            }
            Lifecycle::Advance => Action::Advance(Duration::from_millis(
                [10, 5_000, 60_000][draws.below(3) as usize],
            )),
            Lifecycle::Restart => Action::Restart(node(&mut draws)),
            Lifecycle::Crash => {
                let point = draws.below(RELATIONAL_POINTS.len() as u64 + 1) as usize;
                Action::Crash(node(&mut draws), RELATIONAL_POINTS.get(point).copied())
            }
            Lifecycle::Seal => Action::Seal(signer(&mut draws)),
            Lifecycle::Compact => Action::Compact(signer(&mut draws)),
            Lifecycle::Partition => {
                let list = format!("l{index}");
                lists.push(list.clone());
                Action::Partition(list)
            }
            Lifecycle::Share => {
                let list = list(&mut draws);
                Action::Share(list, other(&mut draws))
            }
            Lifecycle::Invite if members < scenario.nodes => {
                members += 1;
                Action::Invite(draws.below(members as u64 - 1) as usize, members - 1)
            }
            Lifecycle::Revoke if !revoked && members > 2 => {
                revoked = true;
                Action::Revoke(other(&mut draws))
            }
            Lifecycle::Invite | Lifecycle::Revoke => continue,
            Lifecycle::Evict => {
                let list = list(&mut draws);
                Action::Evict(other(&mut draws), list)
            }
            Lifecycle::Hold => {
                let list = list(&mut draws);
                Action::Hold(other(&mut draws), list)
            }
            Lifecycle::Offline => Action::Offline(node(&mut draws)),
            Lifecycle::Online => Action::Online(node(&mut draws)),
        };
        // The lifecycle paths: a member joining after a compacted seal
        // imports a checkpoint, a crash during that import, a share to a
        // new member, and a compaction right after a seal.
        let follow = match &action {
            Action::Seal(0) if draws.chance(500) => Some(Action::Compact(0)),
            Action::Compact(_) if members < scenario.nodes && draws.chance(400) => {
                members += 1;
                Some(Action::Invite(0, members - 1))
            }
            Action::Invite(_, joined) if draws.chance(600) => {
                Some(Action::Share(lists[0].clone(), *joined))
            }
            Action::Share(_, node) if draws.chance(300) => Some(Action::Crash(
                *node,
                [None, Some(CrashPoint::Import), Some(CrashPoint::Switch)][draws.below(3) as usize],
            )),
            _ => None,
        };
        steps.push(action);
        steps.extend(follow);
    }
    steps.truncate(scenario.steps);
    steps
}

fn change(draws: &mut Draws, lists: &[String]) -> Change {
    let list = |draws: &mut Draws| lists[draws.below(lists.len() as u64) as usize].clone();
    let small = |draws: &mut Draws, below: u64| draws.below(below) as u8;
    match draws.below(20) {
        0..=3 => Change::Board {
            list: list(draws),
            code: small(draws, 4),
            // 13 breaks the check where it is written.
            cap: 3 + small(draws, 11),
        },
        4..=8 => Change::Card {
            list: list(draws),
            board: small(draws, 8),
            pinned: draws.chance(500).then(|| small(draws, 8)),
            name: small(draws, 5),
            points: small(draws, 6),
        },
        9 => Change::Pair {
            list: list(draws),
            code: small(draws, 4),
            name: small(draws, 5),
            points: small(draws, 7),
        },
        10..=12 => Change::Raise {
            list: list(draws),
            card: small(draws, 8),
            by: small(draws, 6) as i8 - 1,
        },
        13 => Change::Recode {
            list: list(draws),
            board: small(draws, 8),
            code: small(draws, 4),
        },
        14..=15 => Change::DropBoard {
            list: list(draws),
            board: small(draws, 8),
        },
        16 => Change::DropCard {
            list: list(draws),
            card: small(draws, 8),
        },
        17..=18 => Change::MoveCard {
            from: list(draws),
            to: list(draws),
            card: small(draws, 8),
            board: small(draws, 8),
        },
        _ => Change::MoveBoard {
            from: list(draws),
            to: list(draws),
            board: small(draws, 8),
        },
    }
}
