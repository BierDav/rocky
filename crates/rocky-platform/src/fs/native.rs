use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

pub use std::fs::{
    copy, create_dir_all, read, read_to_string, remove_dir_all, remove_file, rename, write,
};

/// Creates `path` and its parents, owner-only where the platform has modes.
pub fn create_private_dir_all(path: impl AsRef<Path>) -> io::Result<()> {
    let path = path.as_ref();
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(path)
}

/// Replaces `path` atomically with owner-only `bytes`, durable on return.
pub fn write_private(path: impl AsRef<Path>, bytes: &[u8]) -> io::Result<()> {
    let path = path.as_ref();
    let tmp = path.with_extension("tmp");
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(tmp, path)?;
    #[cfg(unix)]
    File::open(
        path.parent()
            .ok_or_else(|| io::Error::other("missing parent"))?,
    )?
    .sync_all()?;
    Ok(())
}

/// The paths of the entries directly in `directory`.
pub fn read_dir(directory: impl AsRef<Path>) -> io::Result<Vec<PathBuf>> {
    let directory = directory.as_ref();
    std::fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect()
}

/// When the file at `path` was last written.
pub fn modified(path: impl AsRef<Path>) -> io::Result<web_time::SystemTime> {
    let path = path.as_ref();
    std::fs::metadata(path)?.modified()
}

/// Whether a file or directory exists at `path`.
pub fn exists(path: impl AsRef<Path>) -> bool {
    let path = path.as_ref();
    path.exists()
}

/// Writes a new owner-only file, durable on return; an existing one fails
/// with `AlreadyExists`, so the first of several writers wins.
pub fn create_new(path: impl AsRef<Path>, bytes: &[u8]) -> io::Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        create_private_dir_all(parent)?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// The length of the file at `path`.
pub fn len(path: impl AsRef<Path>) -> io::Result<u64> {
    let path = path.as_ref();
    Ok(std::fs::metadata(path)?.len())
}

/// Makes the file at `path` durable; `false` if there is none.
pub fn sync(path: impl AsRef<Path>) -> io::Result<bool> {
    let path = path.as_ref();
    // `sync_all` is `F_FULLFSYNC` on Apple platforms, like `fullfsync=1`.
    // Windows requires write access for FlushFileBuffers.
    match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => file.sync_all().map(|()| true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Nothing to prepare: the directory is created on demand.
pub async fn prepare(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        create_dir_all(parent)?;
    }
    Ok(())
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
    pub async fn acquire(path: &Path) -> io::Result<Self> {
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path(path))?;
        // A child process being spawned briefly shares a just-closed owner's
        // lock until it execs, so a reopen right after close waits a moment.
        let mut attempt = 0;
        while let Err(error) = lock.try_lock() {
            attempt += 1;
            if attempt == 10 {
                return Err(io::Error::new(
                    io::ErrorKind::ResourceBusy,
                    format!("already open or file lock unavailable: {error}"),
                ));
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        Ok(Self { _lock: lock })
    }
}

#[cfg(feature = "test-support")]
pub struct Scratch(tempfile::TempDir);
#[cfg(feature = "test-support")]
impl Scratch {
    pub async fn new() -> Self {
        Self(tempfile::tempdir().expect("a temporary directory"))
    }
    pub fn path(&self) -> &Path {
        self.0.path()
    }
}
