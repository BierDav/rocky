//! Consumer fixtures use definition tables and the public SQL APIs.
pub mod secrets;
use serde_json::Value;
pub mod snapshot;
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

#[path = "../../../src/test_directory.rs"]
mod directory;
pub use directory::Directory;

/// Small protocol fixtures keep real SQLite files on Linux's memory filesystem.
pub fn small_directory() -> std::io::Result<Directory> {
    directory::small()
}
