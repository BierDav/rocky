//! Gap-free counters and admit-once claims for Rocky databases, through a Raft coordinator.
#![deny(missing_docs)]
#![cfg_attr(
    not(any(feature = "database", feature = "voter")),
    expect(
        dead_code,
        reason = "without the database or voter feature only the counter and error types are used"
    )
)]
#[cfg(feature = "voter")]
pub use crate::machine::Retention;
pub use certified::TrustBundle;
pub use client::CALL_DEADLINE;
pub use counters::{Claim, Claimed, Name};
#[cfg(feature = "database")]
pub use database::{ControlReceipt, CounterError, Counters};
pub use error::{CoordinationError, Refusal};
#[cfg(feature = "voter")]
pub use retention::Limits;
pub use tenancy::Tenant;

/// Signed configuration: the genesis, control certificates and trust bundles.
#[cfg(feature = "voter")]
pub mod certified;
#[cfg(not(feature = "voter"))]
mod certified;
mod client;
/// Counter names, claims and their limits.
pub mod counters;
#[cfg(feature = "database")]
mod database;
mod error;
#[cfg(feature = "voter")]
mod machine;
mod request;
mod retention;
/// The low-level client and the voters' answers, for voters and operators.
#[cfg(feature = "voter")]
pub mod service;
/// Tenants, owner grants and provider admissions.
pub mod tenancy;
mod transport;

/// The replicated state machine voters run.
#[cfg(feature = "voter")]
pub mod state {
    pub use crate::machine::Metadata;
}

/// The wire protocol between clients and voters.
#[cfg(feature = "voter")]
pub mod protocol {
    pub use crate::request::Request;
    pub use crate::service::Reply;
    #[cfg(not(target_family = "wasm"))]
    pub use crate::transport::exchange;
    pub use crate::transport::{
        Envelope, MAX_CLIENT_FRAME, WireMessage as Message, converse, nonce, read, read_bounded,
        write,
    };
}

#[cfg(all(test, not(target_family = "wasm")))]
mod native_tests;
