//! Protocol-neutral network ports; applications supply their own connector.
pub use rocky_core::net::{
    BoxFuture, Connected, Connector, Discovery, Failure, Found, Incoming, LOCAL_PATH, PathState,
    Protocol, SessionStream, Stream, TOPIC_PATH, Transport,
};

pub mod memory;
