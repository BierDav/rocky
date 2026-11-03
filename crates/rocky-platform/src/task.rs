//! Spawning and joining. Natively these are tokio's; `dedicated` gives a
//! task its own thread. A browser Worker has one thread, so its tasks share
//! the event loop and `spawn_blocking` runs its work in place.
#[cfg_attr(not(target_family = "wasm"), path = "task/native.rs")]
#[cfg_attr(target_family = "wasm", path = "task/browser.rs")]
mod platform;

pub use platform::*;
