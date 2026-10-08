//! Directory identity policy; registration never creates application data grants.
use rocky_core::*;
mod adapter;
mod model;
mod policy;
pub use model::*;
