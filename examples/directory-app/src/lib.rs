//! Directory identity policy as a deterministic transition model;
//! registration never creates application data grants.
use rocky_core::*;
mod model;
mod policy;
pub use model::*;
