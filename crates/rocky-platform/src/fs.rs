//! Files. Natively the filesystem: a path names a file, and an `Owner`
//! holds a `.lock` beside it. In a browser, Rocky's OPFS pool: only a
//! dedicated Worker can open it, and its sync access handles exclude every
//! other worker and tab, so one tab owns an installation at a time.
//! Operations are synchronous on both; a browser must `prepare` first.
#[cfg_attr(not(target_family = "wasm"), path = "fs/native.rs")]
#[cfg_attr(target_family = "wasm", path = "fs/browser.rs")]
mod platform;

pub use platform::*;
