//! Pinned libc overlay: upstream declarations plus the missing iOS Darwin network ABI.
#![no_std]
pub use libc_upstream::*;
#[cfg(target_os = "ios")]
mod ios;
#[cfg(target_os = "ios")]
pub use ios::*;
