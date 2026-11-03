//! A coordinator voter and its setup commands. See
//! `docs/runtime/coordinator-operations.md`. The voter is native only.
#[cfg(not(target_family = "wasm"))]
mod voter;

#[cfg(not(target_family = "wasm"))]
fn main() -> Result<(), voter::Failure> {
    voter::main()
}
#[cfg(target_family = "wasm")]
fn main() {}
