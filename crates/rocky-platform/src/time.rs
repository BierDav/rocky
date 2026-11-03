//! Timers and the monotonic clock: tokio's natively, so tests can pause
//! them, and the browser's in a Worker. `SystemTime` is `web_time`'s, which
//! is `std::time::SystemTime` natively.
#[cfg_attr(not(target_family = "wasm"), path = "time/native.rs")]
#[cfg_attr(target_family = "wasm", path = "time/browser.rs")]
pub(crate) mod platform;

pub use platform::*;
pub use std::time::Duration;
pub use web_time::{SystemTime, UNIX_EPOCH};
