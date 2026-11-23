use crate::{ResourceScope, Scope, commitment};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Blocker {
    pub scope: String,
    pub kind: BlockerKind,
    pub clears_by: ClearsBy,
    pub since: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BlockerKind {
    BootstrapMarker,
    SyncRequest {
        refused: bool,
    },
    AwaitingInviter,
    Importing,
    StagedImport {
        sealed: bool,
        pages_complete: bool,
    },
    LostCompaction,
    SealRebootstrap {
        replay: FrameReplay,
        listed: u64,
    },
    RestorationGuard,
    LineNode,
    PendingFrames {
        reason: WaitReason,
        count: u64,
    },
    Quarantined {
        reason: String,
        count: u64,
    },
    FinalizationWaiting {
        reason: String,
    },
    ClockAhead {
        node: String,
        count: u64,
        seconds: u64,
    },
    AwaitingPeerFrontier,
    OwnLogBehind {
        held: u64,
        reported: u64,
    },
    StoreSuperseded {
        held: u64,
        reported: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameReplay {
    Local,
    Peers,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClearsBy {
    AnyPeer,
    Peer { node: String },
    LocalWork,
    Application,
    NoPeer,
    Clock,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitReason {
    Schema,
    DirectImport,
    OpeningKey,
    ReadGrant,
    OwnChain,
    Dependencies,
    Undecryptable,
    Ledger,
    Failed,
    Quarantined,
    Unexamined,
    ClockAhead,
}

impl WaitReason {
    pub fn clears_by(self) -> ClearsBy {
        match self {
            Self::Schema | Self::ReadGrant => ClearsBy::Application,
            Self::Ledger | Self::Failed | Self::Quarantined | Self::Unexamined => {
                ClearsBy::LocalWork
            }
            Self::DirectImport
            | Self::OpeningKey
            | Self::OwnChain
            | Self::Dependencies
            | Self::Undecryptable => ClearsBy::AnyPeer,
            Self::ClockAhead => ClearsBy::Clock,
        }
    }
}

pub fn scope_label(scope: &ResourceScope) -> String {
    label("rocky.blocker.scope.v1", serde_json::to_vec(scope))
}
pub fn domain_label(domain: &Scope) -> String {
    label("rocky.blocker.domain.v1", serde_json::to_vec(domain))
}
fn label(prefix: &str, bytes: Result<Vec<u8>, serde_json::Error>) -> String {
    let hash = commitment(prefix, &bytes.unwrap_or_default());
    hash[..6].iter().map(|byte| format!("{byte:02x}")).collect()
}

impl fmt::Display for Blocker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match &self.kind {
            BlockerKind::SyncRequest { refused } => format!(
                "sync request {}",
                if *refused { "refused" } else { "failed" }
            ),
            BlockerKind::BootstrapMarker => "bootstrap marker".into(),
            BlockerKind::AwaitingInviter => "awaiting inviter".into(),
            BlockerKind::Importing => "importing".into(),
            BlockerKind::StagedImport {
                sealed,
                pages_complete,
            } => format!("staged import (sealed={sealed}, pages complete={pages_complete})"),
            BlockerKind::LostCompaction => "lost compaction".into(),
            BlockerKind::SealRebootstrap { replay, listed } => {
                format!("seal rebootstrap (replay={replay:?}, listed={listed})")
            }
            BlockerKind::RestorationGuard => "restoration guard".into(),
            BlockerKind::LineNode => "no node of this identity on the followed line".into(),
            BlockerKind::PendingFrames { reason, count } => {
                format!("{count} pending frames ({reason:?})")
            }
            BlockerKind::Quarantined { reason, count } => {
                format!("{count} quarantined frames ({reason})")
            }
            BlockerKind::FinalizationWaiting { reason } => {
                format!("finalization waiting ({reason})")
            }
            BlockerKind::ClockAhead {
                node,
                count,
                seconds,
            } => format!(
                "{count} frames of {} up to {seconds} s ahead",
                node.get(..12).unwrap_or(node)
            ),
            BlockerKind::AwaitingPeerFrontier => {
                "no peer has reported this device's log since it opened".into()
            }
            BlockerKind::OwnLogBehind { held, reported } => format!(
                "this device holds {held} of the {reported} positions peers report for its key"
            ),
            BlockerKind::StoreSuperseded { held, reported } => format!(
                "this device's store is older than what peers hold for its key ({held} of {reported} positions); re-invite it as a new device"
            ),
        };
        let clears = match &self.clears_by {
            ClearsBy::AnyPeer => "any permitted peer".into(),
            ClearsBy::Peer { node } => format!("peer {}", node.get(..12).unwrap_or(node)),
            ClearsBy::LocalWork => "local work".into(),
            ClearsBy::Application => "the application".into(),
            ClearsBy::Clock => "this device's clock".into(),
            ClearsBy::NoPeer => return write!(f, "{} {kind}: no peer can serve", self.scope),
        };
        write!(f, "{} {kind}, cleared by {clears}", self.scope)
    }
}

pub fn describe_blockers(blockers: &[Blocker]) -> String {
    let list: Vec<String> = blockers.iter().map(ToString::to_string).collect();
    format!(
        "the workspace waits for {} blockers: {}",
        blockers.len(),
        list.join("; ")
    )
}
