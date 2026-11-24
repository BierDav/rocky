//! Gap-free counters and at-most-once claims for Rocky databases, through a Raft coordinator.
#![deny(missing_docs)]
#[cfg(feature = "database")]
pub use database::{CounterError, Counters};
pub use error::{CoordinationError, Refusal};
pub use retention::Limits;
use rocky_core::*;

/// What a coordinator's counters and registrations hold.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Retention {
    /// Registered databases.
    pub domains: usize,
    /// Accounted bytes.
    pub bytes: usize,
}

/// Signed configuration: the genesis, control certificates and trust bundles.
pub mod certified;
mod client;
/// Registering and renewing databases as an operator.
pub mod control;
#[cfg(feature = "database")]
mod database;
mod error;
mod machine;
mod retention;
/// The low-level client and the voters' answers.
pub mod service;
/// Tenants, owner grants and provider admissions.
pub mod tenancy;
mod transport;

/// The replicated state machine voters run.
pub mod state {
    pub use crate::machine::Metadata;
}

/// The wire protocol between clients and voters.
pub mod protocol {
    pub use crate::machine::Request;
    pub use crate::service::Reply;
    #[cfg(not(target_family = "wasm"))]
    pub use crate::transport::exchange;
    pub use crate::transport::{
        Envelope, MAX_CLIENT_FRAME, WireMessage as Message, converse, nonce, read, read_bounded,
        write,
    };
}

/// Counter names, claims and readings.
pub mod counters;

#[cfg(all(test, not(target_family = "wasm")))]
mod native_tests;
