//! SQLite access, row capture and errors shared by the runtimes. An internal
//! boundary of the workspace; see `docs/modules.md`.
pub mod capture;
pub mod cursor;
pub mod database;
pub mod digest;
pub mod observe;
pub mod parsed;
pub mod partition_sql;
pub mod scoped_connection;
pub mod sql;
pub mod sqlite;
pub mod statement;
pub mod storage;
pub use storage::{Error, FailureReason, StorageFailure};

/// Largest single frame or payload a replica accepts.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
/// How far a commit had reached when it was reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitClass {
    /// Committed locally without a device flush. Power loss may drop it; that
    /// is safe because nothing outside the device has seen it.
    Buffered,
    /// Covered by a full flush: at or below the file's durable watermark.
    Flushed,
}
