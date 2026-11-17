#[cfg_attr(not(target_family = "wasm"), path = "time/native.rs")]
#[cfg_attr(target_family = "wasm", path = "time/browser.rs")]
pub(crate) mod platform;

pub use platform::*;
pub use std::time::Duration;
pub use web_time::{SystemTime, UNIX_EPOCH};

pub fn since_epoch() -> Result<Duration, web_time::SystemTimeError> {
    #[cfg(feature = "simulation")]
    if let Some(time) = crate::entropy::simulation::since_epoch() {
        return Ok(time);
    }
    let time = SystemTime::now().duration_since(UNIX_EPOCH)?;
    #[cfg(feature = "simulation")]
    let time = time + crate::entropy::simulation::skipped();
    Ok(time)
}
