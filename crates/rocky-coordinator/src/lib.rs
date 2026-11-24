#[cfg(feature = "database")]
pub use database::{CounterError, Counters};
pub use error::{CoordinationError, Refusal};
pub use retention::Limits;
use rocky_core::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Retention {
    pub domains: usize,
    pub bytes: usize,
}

pub mod certified;
mod client;
pub mod control;
#[cfg(feature = "database")]
mod database;
mod error;
mod machine;
mod retention;
pub mod service;
pub mod tenancy;
mod transport;

pub mod state {
    pub use crate::machine::Metadata;
}

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

pub mod counters;

#[cfg(all(test, not(target_family = "wasm")))]
mod native_tests;
