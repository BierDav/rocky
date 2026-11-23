pub mod capture;
pub mod cursor;
pub mod database;
pub mod exact;
pub mod files;
pub mod observe;
pub mod parsed;
pub mod partition_sql;
pub mod scoped_connection;
pub mod sql;
pub mod sqlite;
pub mod statement;
pub mod storage;
#[cfg(all(any(test, feature = "test-support"), not(target_family = "wasm")))]
pub use rocky_sqlite::wal_recoveries;
pub use storage::{Error, FailureReason, StorageFailure};

pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitClass {
    Buffered,
    Flushed,
}
