use crate::Error;
use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
};

/// Creates the directory that will hold `path`.
pub async fn prepare(path: &Path) -> Result<(), Error> {
    if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(Error::Io)?;
    }
    Ok(())
}

/// Whether `path` exists and is not empty.
pub fn has_data(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|file| file.len() > 0)
}

pub(crate) fn connect(
    path: &Path,
    flags: rusqlite::OpenFlags,
) -> rusqlite::Result<rusqlite::Connection> {
    rusqlite::Connection::open_with_flags(path, flags)
}

/// `<file>.lock` beside the file: appended, so `a.db` and `a.sqlite` in one
/// directory do not share a lock.
fn lock_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

/// The lock file excludes a second process, so one writer owns the file.
/// Holding it holds the lock.
pub struct Owner {
    _lock: File,
}
impl Owner {
    pub async fn acquire(path: &Path) -> Result<Self, Error> {
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path(path))
            .map_err(Error::Io)?;
        // A child process being spawned briefly shares a just-closed owner's
        // lock until it execs, so a reopen right after close waits a moment.
        let mut attempt = 0;
        while let Err(error) = lock.try_lock() {
            attempt += 1;
            if attempt == 10 {
                return Err(Error::Incompatible(format!(
                    "replica is already open or file lock unavailable: {error}"
                )));
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        Ok(Self { _lock: lock })
    }
}

#[cfg(any(test, feature = "test-support"))]
pub struct Scratch(tempfile::TempDir);
#[cfg(any(test, feature = "test-support"))]
impl Scratch {
    pub async fn new() -> Self {
        Self(tempfile::tempdir().expect("a temporary directory"))
    }
    pub fn path(&self) -> &Path {
        self.0.path()
    }
}
