#[cfg(not(target_family = "wasm"))]
mod public_check;
#[cfg(not(target_family = "wasm"))]
mod relay;
#[cfg(not(target_family = "wasm"))]
mod voter;

#[cfg(not(target_family = "wasm"))]
fn main() -> Result<(), voter::Failure> {
    use tracing_subscriber::{filter::Targets, layer::SubscriberExt, util::SubscriberInitExt};
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_ansi(false),
        )
        .with(
            Targets::new()
                .with_default(tracing::Level::WARN)
                .with_target("rocky_voter", tracing::Level::INFO),
        )
        .init();
    voter::main()
}
#[cfg(target_family = "wasm")]
fn main() {}
