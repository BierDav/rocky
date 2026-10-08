//! The `turso-engine` adapter against patched turso_core; see ../prepare.sh.
#[path = "../../crates/turso-engine/src/adapter.rs"]
mod adapter;

const NAME: &str = "turso-patched";
const VERSION: &str = "0.8.1 mvcc + root-page index";

fn main() -> harness::Result<()> {
    harness::run::<adapter::Turso>()
}
