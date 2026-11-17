//! Bounded RAM-backed directories for small protocol fixtures.
use std::{fs::File, ops::Deref};

pub struct Directory {
    /// Declared first, so the directory drops before its reservation.
    root: tempfile::TempDir,
    /// The cross-process lock on a RAM slot, released after the directory.
    _reservation: Option<File>,
}

impl Deref for Directory {
    type Target = tempfile::TempDir;

    fn deref(&self) -> &Self::Target {
        &self.root
    }
}

impl From<tempfile::TempDir> for Directory {
    fn from(root: tempfile::TempDir) -> Self {
        Self {
            root,
            _reservation: None,
        }
    }
}

/// RAM slots shared by all fixtures: Docker's default /dev/shm is only
/// 64 MiB, so confirmations, which run alone, use RAM without every parallel
/// fixture occupying it.
const RAM_SLOTS: usize = 3;

/// A small fixture directory, in RAM when a slot is free on Linux.
pub fn small() -> std::io::Result<Directory> {
    if cfg!(target_os = "linux") {
        for slot in 0..RAM_SLOTS {
            let reservation = File::options()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(format!("/dev/shm/rocky-fixture-slot-{slot}.lock"));
            if let Ok(reservation) = reservation
                && reservation.try_lock().is_ok()
                && let Ok(root) = tempfile::Builder::new()
                    .prefix("rocky-fixture-")
                    .tempdir_in("/dev/shm")
            {
                return Ok(Directory {
                    root,
                    _reservation: Some(reservation),
                });
            }
        }
    }
    tempfile::tempdir().map(Directory::from)
}
