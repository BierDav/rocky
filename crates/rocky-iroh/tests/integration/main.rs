//! The transport suites, one test binary. Loopback endpoints and a local
//! relay bind sockets, which a browser has not.
#![cfg(not(target_family = "wasm"))]
mod discovery;
mod ownership;
