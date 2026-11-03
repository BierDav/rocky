//! Timers and the monotonic clock: tokio's natively, so tests can pause
//! them, and the browser's in a Worker. `SystemTime` is `web_time`'s, which
//! is `std::time::SystemTime` natively.
#[cfg_attr(not(target_family = "wasm"), path = "time/native.rs")]
#[cfg_attr(target_family = "wasm", path = "time/browser.rs")]
pub(crate) mod platform;

pub use platform::*;
pub use std::time::Duration;
pub use web_time::{SystemTime, UNIX_EPOCH};

/// Time since the Unix epoch on the wall clock, or the simulated clock of
/// a thread that installed one (`entropy::simulation`).
pub fn since_epoch() -> Result<Duration, web_time::SystemTimeError> {
    #[cfg(feature = "simulation")]
    if let Some(time) = crate::entropy::simulation::since_epoch() {
        return Ok(time);
    }
    SystemTime::now().duration_since(UNIX_EPOCH)
}
