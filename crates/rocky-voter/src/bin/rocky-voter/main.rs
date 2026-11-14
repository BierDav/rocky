//! A coordinator voter, relay and setup commands. Native only.
#[cfg(not(target_family = "wasm"))]
mod public_check;
#[cfg(not(target_family = "wasm"))]
mod relay;
#[cfg(not(target_family = "wasm"))]
mod voter;

#[cfg(not(target_family = "wasm"))]
fn main() -> Result<(), voter::Failure> {
    voter::main()
}
#[cfg(target_family = "wasm")]
fn main() {}
