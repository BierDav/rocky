pub mod secrets;
use serde_json::Value;
pub mod snapshot;
pub const CONTRACT_SCOPE: rocky_core::Scope = rocky_core::Scope {
    mesh: rocky_core::MeshId([1; 16]),
    app: rocky_core::AppId([2; 16]),
    domain: rocky_core::DomainId([3; 16]),
};
pub fn contract_database() -> rocky_sdk::credentials::DatabaseContext {
    rocky_sdk::credentials::DatabaseContext {
        publisher: rocky_sdk::credentials::PrivateKey::new().public_key(),
        scope: CONTRACT_SCOPE,
    }
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

#[path = "../../../src/test_directory.rs"]
mod directory;
pub use directory::Directory;

pub fn small_directory() -> std::io::Result<Directory> {
    directory::small()
}
