//! Turso 0.8.1 in MVCC mode, as Rocky uses it.
mod adapter;

const NAME: &str = "turso";
const VERSION: &str = "0.8.1 mvcc";

fn main() -> harness::Result<()> {
    harness::run::<adapter::Turso>()
}
