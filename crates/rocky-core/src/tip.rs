//! Tip identity shared by the generation graph and per-tip coordinated logs.
use crate::Commitment;

/// One tip of a domain's generation graph: a schema and the frames written
/// to it. The id is a digest fixed by the statement that opens the tip, so
/// every device names it alike; the coordinator treats it as opaque.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct TipId(pub Commitment);
