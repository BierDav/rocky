//! A coordinator voter, optional relay and setup commands. Native only.
#[cfg(all(not(target_family = "wasm"), feature = "iroh"))]
mod public_check;
#[cfg(all(not(target_family = "wasm"), feature = "relay-server"))]
mod relay;
#[cfg(not(target_family = "wasm"))]
mod voter;

#[cfg(not(target_family = "wasm"))]
fn main() -> Result<(), voter::Failure> {
    voter::main()
}
#[cfg(target_family = "wasm")]
fn main() {}
