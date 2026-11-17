#[cfg_attr(not(target_family = "wasm"), path = "fs/native.rs")]
#[cfg_attr(target_family = "wasm", path = "fs/browser.rs")]
mod platform;

pub use platform::*;
