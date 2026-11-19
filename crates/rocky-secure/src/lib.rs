#[macro_use]
mod transaction;
use rocky_storage::{
    capture, cursor, database, observe, scoped_connection, sql, sqlite, statement, storage,
};
pub mod network;
mod replica;
mod store;
mod wire;
mod writer;
#[cfg(feature = "qualification")]
pub use replica::CHUNK_PAGES;
#[cfg(any(test, feature = "qualification"))]
pub use replica::CrashPoint;
pub use replica::{
    AuditReport, Buffered, COORDINATOR_MISMATCH, COORDINATOR_REQUIRED, CheckpointPage,
    CheckpointState, CheckpointStatement, CheckpointStatus, CiphertextPage, CompactReport,
    CompactionStatus, Companion, CoveredMark, CurrentValue, CustodySummary, Describe,
    EarlierEpochWrites, EncryptedReplica, EngineStatus, EvictReport, FinalityStall, Finalization,
    FrameReplay, FrontierLog, GenerationStatus, HeldChange, HeldField, HeldKind, HeldRow, HeldSeal,
    Holding, InstalledTipSchema, KeptSeals, KeyMaterial, LineNode, LogPosition, MAX_BUNDLE_BYTES,
    MAX_BUNDLE_FRAMES, MAX_BUNDLE_ROWS, MAX_CHECKPOINT_LOGS, MAX_PLACEMENTS, MAX_SEALED_OUTCOMES,
    MAX_SNAPSHOT_BYTES, MAX_SNAPSHOT_ROWS, MAX_UNCONFIGURED_PEERS, MigrationMembership,
    MigrationStorage, OpenConfig, OutcomeDivergence, PUBLICATION_CHUNK_BUNDLES, PartitionCustody,
    PeerLag, PeerScopeFrontier, PendingSchema, Placement, PlacementPage, PlacementRequest,
    ProjectionPlan, PruneBlock, PruneReport, PrunedPage, Publication, ROWS_PER_FRAME, RangePage,
    RebootstrapReport, ReceiveStatus, Refusal, RemotePage, ReplicaRole, ReplicaStatus,
    ResetEndsSigner, RetentionLimits, RetentionPolicy, RoleConfig, SchemaConfig, SchemaStage,
    ScopeFrontier, ScopeStatus, SealRebootstrap, SealViews, SealedOutcome, SealedOutcomes,
    SealedTransaction, SecureCommit, SecureConfig, SecurePage, SecureRows, SecureSession,
    SecureSnapshot, SecureSql, SecureTransaction, SessionStatus, SetSummary, SignedCheckpoint,
    SignedVoid, StagedCloseStatus, StateAdvice, TipDevice, TipNodeObservation, TipObservation,
    TipStatement, UpdateMode, created_name, installed_tip_schema,
};
pub use rocky_core::{Blocker, BlockerKind, ClearsBy, WaitReason, domain_label, scope_label};
pub use rocky_credentials as credentials;
pub use rocky_credentials::{
    Capability, CredentialChain, DatabaseKind, Hash, MAX_CREDENTIAL_DEPTH, PrivateKey, PublicKey,
    Revocation, RevocationSet, SignedCapability, SignedManifest, SignedRevocation,
};
pub use rocky_storage::sql::{
    AbortReason, Certainty, MAX_SAVEPOINTS, MAX_TRANSACTION_TIMEOUT, SqlError, SqlOptions,
    SqlState, Unavailable,
};
pub use rocky_storage::{CommitClass, Error, FailureReason, MAX_FRAME_BYTES, StorageFailure};
use serde::{Serialize, de::DeserializeOwned};
pub use wire::{EncryptedFrame, EncryptedPackage, PackageKind};

pub const MAX_SECURE_FRAME: usize = 256 * 1024;
pub(crate) const SECURE_MESSAGE_BYTES: usize = MAX_SECURE_FRAME - 4096;
fn page_len<T: Serialize>(items: &[T], limit: usize) -> Result<usize, Error> {
    let mut bytes = 1024;
    let mut count = 0;
    for item in items.iter().take(limit) {
        let size = encode(item)?.len();
        if bytes + size > SECURE_MESSAGE_BYTES {
            if count == 0 {
                return Err(Error::Quota {
                    resource: "message item",
                    limit: SECURE_MESSAGE_BYTES - 1024,
                });
            }
            break;
        }
        bytes += size;
        count += 1;
    }
    Ok(count)
}
pub const MAX_SECURE_RECORDS: usize = 4096;
pub const MAX_SECURE_BYTES: usize = 32 * 1024 * 1024;
pub fn secure_node_id(key: PublicKey) -> rocky_core::NodeId {
    rocky_credentials::PeerId::of(key).node_id()
}
fn denied(message: &str) -> Error {
    Error::Rejected(message.into())
}
fn credential(error: rocky_credentials::Error) -> Error {
    match error {
        rocky_credentials::Error::Protocol(error) => Error::Protocol(error),
        unsupported @ rocky_credentials::Error::UnsupportedFormat { .. } => {
            Error::Incompatible(format!("this store or peer holds {unsupported}"))
        }
        other => Error::Rejected(other.to_string()),
    }
}
fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, Error> {
    p2panda_core::cbor::encode_cbor(value).map_err(|_| denied("invalid canonical encoding"))
}
fn decode<T: Serialize + DeserializeOwned>(bytes: &[u8]) -> Result<T, Error> {
    decode_bounded(bytes, MAX_SECURE_FRAME)
}
fn decode_bounded<T: Serialize + DeserializeOwned>(bytes: &[u8], limit: usize) -> Result<T, Error> {
    if bytes.len() > limit {
        return Err(denied("secure frame quota"));
    }
    let value: T =
        p2panda_core::cbor::decode_cbor(bytes).map_err(|_| denied("invalid secure frame"))?;
    if encode(&value)? != bytes {
        return Err(denied("non-canonical secure frame"));
    }
    Ok(value)
}
fn signing_bytes<T: Serialize>(label: &str, value: &T) -> Result<Vec<u8>, Error> {
    encode(&(label, value))
}
fn micros() -> Result<u64, Error> {
    let time = rocky_platform::time::since_epoch()
        .map(|time| u64::try_from(time.as_micros()).unwrap_or(u64::MAX))
        .map_err(|_| denied("clock before epoch"))?;
    #[cfg(any(test, feature = "qualification"))]
    let time = time.saturating_add(
        CLOCK_SKEW
            .with(std::cell::Cell::get)
            .saturating_mul(1_000_000),
    );
    Ok(time)
}
fn next_clock(observed: u64) -> Result<u64, Error> {
    next_clock_after(observed, micros)
}
fn next_clock_after(
    observed: u64,
    wall: impl FnOnce() -> Result<u64, Error>,
) -> Result<u64, Error> {
    observed
        .checked_add(1)
        .map(|clock| wall().map(|wall| clock.max(wall)))
        .transpose()?
        .filter(|clock| *clock <= rocky_core::MAX_CLOCK)
        .ok_or_else(|| denied("hybrid clock beyond its bound"))
}
fn now() -> Result<u64, Error> {
    let time = rocky_platform::time::since_epoch()
        .map(|time| time.as_secs())
        .map_err(|_| denied("clock before epoch"))?;
    #[cfg(any(test, feature = "qualification"))]
    let time = time.saturating_add(CLOCK_SKEW.with(std::cell::Cell::get));
    Ok(time)
}
#[cfg(any(test, feature = "qualification"))]
thread_local! {
    static CLOCK_SKEW: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}
#[cfg(feature = "qualification")]
pub fn skew_clock(seconds: u64) -> u64 {
    CLOCK_SKEW.with(|skew| skew.replace(seconds))
}
#[cfg(test)]
thread_local! {
    static APPLY_BUDGET: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}
#[cfg(test)]
fn apply_allowed() -> bool {
    APPLY_BUDGET.with(|budget| match budget.get() {
        Some(0) => false,
        Some(left) => {
            budget.set(Some(left - 1));
            true
        }
        None => true,
    })
}

#[cfg(test)]
#[cfg_attr(
    target_family = "wasm",
    expect(
        dead_code,
        reason = "Crash and thread tests run natively, leaving shared helpers unused in the browser."
    )
)]
mod tests;

pub use replica::checkpoint::IMPORT_ATTEMPTS;
#[doc(hidden)]
pub use replica::last_in_place_switch_changes;
pub use replica::replay::ROUTINE_SEAL_TRANSACTIONS;
pub use replica::{check_replica_format, local_row_counts, local_table_digests};

pub use rocky_credentials::PeerId;

#[cfg(feature = "qualification")]
pub use replica::{
    HolderMeasurements, LockMeasurements, Measurements, with_measurement_connection,
};

#[cfg(feature = "qualification")]
fn qualification_bound(name: &str, default: usize, maximum: usize) -> Result<usize, Error> {
    match std::env::var_os(name) {
        None => Ok(default),
        Some(value) => value
            .to_str()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|bound| (1..=maximum).contains(bound))
            .ok_or_else(|| denied(&format!("{name} must be between 1 and {maximum}"))),
    }
}

#[cfg(test)]
mod clock_tests {
    use super::*;

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn the_hybrid_clock_passes_every_observed_clock_and_never_falls_behind_the_wall() {
        let wall = 5_000;
        let at = |observed| next_clock_after(observed, || Ok(wall));
        assert_eq!(at(0).unwrap(), wall);
        assert_eq!(at(wall - 1).unwrap(), wall);
        assert_eq!(at(wall).unwrap(), wall + 1);
        assert_eq!(at(9_000).unwrap(), 9_001);
        assert_eq!(
            at(rocky_core::MAX_CLOCK - 1).unwrap(),
            rocky_core::MAX_CLOCK
        );
        assert!(at(rocky_core::MAX_CLOCK).is_err());
        assert!(at(u64::MAX).is_err());
        assert!(
            next_clock_after(0, || Ok(rocky_core::MAX_CLOCK + 1)).is_err(),
            "a wall clock beyond the bound is refused too"
        );
    }
}
