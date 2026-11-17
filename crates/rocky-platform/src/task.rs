#[cfg_attr(not(target_family = "wasm"), path = "task/native.rs")]
#[cfg_attr(target_family = "wasm", path = "task/browser.rs")]
mod platform;

pub use platform::*;
