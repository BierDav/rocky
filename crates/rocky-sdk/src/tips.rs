//! The JSON contract of seals, tips and bookmarks. Digests and tips are
//! lowercase hex; see `docs/runtime/tips-and-bookmarks.md`.
use serde::{Deserialize, Serialize};

/// The followed bookmark and tip, progress since the last seal and each
/// peer's last reported frontier.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TipStatus {
    /// None until this device follows a bookmark.
    pub bookmark: Option<String>,
    pub tip: String,
    pub operations_since_seal: u64,
    /// What compaction would release; null until lazy compaction exists.
    pub evidence_releasable: Option<TipEvidence>,
    pub devices: Vec<TipDevice>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TipEvidence {
    pub operations: u64,
    pub bytes: u64,
}
/// A peer: the tip its open link claims, if any, and its last frontiers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TipDevice {
    pub node: String,
    pub tip: Option<String>,
    pub frontiers: Vec<TipScopeFrontier>,
}
/// One resource scope, as its JSON encoding, and per-author log positions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TipScopeFrontier {
    pub scope: String,
    pub logs: Vec<TipFrontierLog>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TipFrontierLog {
    pub author: String,
    pub held: u64,
    pub applied: u64,
}
/// The last included global position of one author number.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TipAuthorMark {
    pub author: u32,
    pub sequence: u64,
}
/// A seal; a `continue` seal's id is also its successor tip.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TipSeal {
    pub id: String,
    pub tip: String,
    pub frontier: Vec<TipAuthorMark>,
    /// `continue` or `close`.
    pub action: String,
    /// The successor's schema of a `continue` seal.
    pub schema: Option<String>,
    /// False for a concurrent seal of the same tip that lost.
    pub canonical: bool,
}
/// An opened tip; its id is the tip.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TipOpening {
    pub id: String,
    pub seal: String,
    pub minimum_in_sync: u32,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookmarkMove {
    pub id: String,
    pub from: Option<String>,
    pub to: Option<String>,
}
/// A bookmark's resolved tip, none once deleted, and every move of it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bookmark {
    pub name: String,
    pub tip: Option<String>,
    pub moves: Vec<BookmarkMove>,
}
/// What a node follows and keeps; null where this device cannot know.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TipNode {
    pub node: String,
    pub followed_tip: Option<String>,
    pub kept_tips: Option<Vec<String>>,
    pub kept_seals: Option<Vec<String>>,
}
/// The domain's complete graph, for developers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TipInspection {
    /// The domain scope, as its JSON encoding.
    pub scope: String,
    pub genesis: String,
    /// The genesis tip's schema digest.
    pub schema: String,
    pub seals: Vec<TipSeal>,
    pub openings: Vec<TipOpening>,
    pub bookmarks: Vec<Bookmark>,
    pub nodes: Vec<TipNode>,
}
