//! Bounded RAM-backed directories for small protocol fixtures.
use std::{fs::File, ops::Deref};

pub struct Directory {
    root: tempfile::TempDir,
    // Drop the directory before releasing its cross-process reservation.
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

pub fn small() -> std::io::Result<Directory> {
    if cfg!(target_os = "linux") {
        // Docker's default /dev/shm is only 64 MiB. Confirmations run alone,
        // so they can use RAM without every parallel fixture occupying it.
        for slot in 0..3 {
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
