//! Failure-only observations of the unchanged rollback experiment.
use super::*;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
#[path = "diagnostics/cbor.rs"]
mod cbor;
#[path = "diagnostics/snapshot.rs"]
mod snapshot;

pub(super) fn now() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
fn hex(bytes: &[u8]) -> String {
    data_encoding::HEXLOWER.encode(bytes)
}
fn digest(bytes: &[u8]) -> Value {
    json!({"length":bytes.len(),"sha256":hex(&Sha256::digest(bytes))})
}

pub(super) struct Trace {
    root: Option<tempfile::TempDir>,
    pub(super) phase: String,
    last_success: String,
    pub(super) m_sync: Option<Value>,
}
impl Trace {
    pub(super) fn new(root: tempfile::TempDir) -> Self {
        Self {
            root: Some(root),
            phase: String::new(),
            last_success: String::new(),
            m_sync: None,
        }
    }
    pub(super) fn path(&self) -> &Path {
        self.root.as_ref().expect("fixture directory").path()
    }
    pub(super) fn before(&mut self, label: &str) {
        self.phase = label.to_owned();
        eprintln!("rollback diagnostic: before={label} unix_ms={}", now());
    }
    pub(super) fn after(&mut self) {
        eprintln!(
            "rollback diagnostic: after={} unix_ms={}",
            self.phase,
            now()
        );
        self.last_success.clone_from(&self.phase);
    }
    pub(super) fn retain(&mut self) -> Option<std::path::PathBuf> {
        let path = self.root.take()?.keep();
        eprintln!(
            "rollback diagnostic: fixture retained at {}",
            path.display()
        );
        Some(path)
    }
    pub(super) async fn failure(&mut self, error: &Failure, peers: &[(&str, &Peer)]) {
        eprintln!(
            "rollback diagnostic: error_phase={} last_success={} unix_ms={} primary_error={error} preceding_m_sync={:?}",
            self.phase,
            self.last_success,
            now(),
            self.m_sync
        );
        let Some(path) = self.retain() else {
            return;
        };
        let mut dependencies = BTreeSet::new();
        let mut snapshots = json!({"failure_unix_ms":now(),"phase":self.phase,"primary_error":error.to_string(),
            "preceding_m_sync":self.m_sync,"warning":"post-error observations do not identify the disjunct at refusal; progress can remove pending/guards"});
        for name in ["m", "a", "t"] {
            snapshots[name] = match snapshot::capture(&path.join(name), &mut dependencies) {
                Ok(value) => value,
                Err(error) => json!({"diagnostic_error":error.to_string(),"unix_ms":now()}),
            };
        }
        eprintln!("rollback diagnostic: SQL snapshots={snapshots}");
        if let Err(error) = std::fs::write(
            path.join("rollback-diagnostics.json"),
            snapshots.to_string(),
        ) {
            eprintln!("rollback diagnostic: snapshot save error={error}");
        }
        // These public observations are later, non-atomic and never drive progress.
        for (name, peer) in peers {
            for request in [
                json!({"type":"status"}),
                json!({"type":"tips","action":"status"}),
                json!({"type":"tips","action":"inspect"}),
                json!({"type":"tips","action":"compaction"}),
            ] {
                let result =
                    tokio::time::timeout(Duration::from_secs(2), peer.call(request.clone())).await;
                eprintln!(
                    "rollback diagnostic: later_public peer={name} unix_ms={} request={request} result={result:?}",
                    now()
                );
            }
        }
    }
}
