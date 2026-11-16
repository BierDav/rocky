//! The voter suites, one test binary. Voters are native: Raft, file stores and sockets.
#![cfg(not(target_family = "wasm"))]
mod cli;
mod cluster;
mod coordinated_log;
mod public;
mod support;
