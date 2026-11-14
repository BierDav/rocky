//! Consumer fixtures use definition tables and the public SQL APIs.
#![expect(
    dead_code,
    reason = "shared fixture helpers vary between integration suites"
)]
pub mod secrets;
use serde_json::Value;
pub mod snapshot;
/// Stable identities for deterministic strong-command fixture reducers.
pub fn identity_row(mut row: Vec<rocky_core::SqlValue>) -> Vec<rocky_core::SqlValue> {
    let sequence = format!("{:?}", row[0]).bytes().fold(1u32, |n, byte| {
        n.wrapping_mul(31).wrapping_add(u32::from(byte))
    }) % 1_000_000
        + 1;
    row.extend([
        rocky_core::SqlValue::Integer(4294967296 + i64::from(sequence)),
        rocky_core::SqlValue::Integer(0),
    ]);
    row
}

pub fn reconciled(report: &Value) -> bool {
    report["peers"].as_array().is_some_and(|peers| {
        peers.iter().any(|peer| {
            peer["attempts"].as_array().is_some_and(|attempts| {
                attempts.iter().any(|attempt| {
                    attempt["operation"] == "reconcile" && attempt["outcome"] == "completed"
                })
            })
        })
    })
}

pub fn completed_peers(report: &Value) -> usize {
    report["peers"]
        .as_array()
        .expect("native peer reports")
        .iter()
        .filter(|peer| {
            peer["attempts"]
                .as_array()
                .expect("native attempts")
                .iter()
                .any(|attempt| {
                    attempt["operation"] == "reconcile" && attempt["outcome"] == "completed"
                })
        })
        .count()
}

#[path = "../../src/test_directory.rs"]
mod directory;
pub use directory::Directory;

/// Small protocol fixtures keep real SQLite files on Linux's memory filesystem.
pub fn small_directory() -> std::io::Result<Directory> {
    directory::small()
}
