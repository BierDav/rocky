use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

pub use std::fs::{copy, create_dir_all, read, read_to_string, remove_dir_all, remove_file, write};

fn parent(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

pub fn sync_dir(directory: impl AsRef<Path>) -> io::Result<()> {
    let directory = directory.as_ref();
    #[cfg(test)]
    tests::SYNCED.with(|synced| synced.borrow_mut().push(directory.to_path_buf()));
    #[cfg(unix)]
    File::open(directory)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = directory;
    Ok(())
}

pub fn create_private_dir_all(path: impl AsRef<Path>) -> io::Result<()> {
    let path = path.as_ref();
    if path.as_os_str().is_empty() || path.is_dir() {
        return Ok(());
    }
    create_private_dir_all(parent(path))?;
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    match builder.create(path) {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        created => {
            created?;
            sync_dir(parent(path))
        }
    }
}

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
    sync_dir(parent(path))
}

pub fn rename(from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<()> {
    let (from, to) = (from.as_ref(), to.as_ref());
    std::fs::rename(from, to)?;
    sync_dir(parent(to))?;
    if parent(from) != parent(to) {
        sync_dir(parent(from))?;
    }
    Ok(())
}

pub fn read_dir(directory: impl AsRef<Path>) -> io::Result<Vec<PathBuf>> {
    let directory = directory.as_ref();
    std::fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect()
}

pub fn modified(path: impl AsRef<Path>) -> io::Result<web_time::SystemTime> {
    let path = path.as_ref();
    std::fs::metadata(path)?.modified()
}

pub fn exists(path: impl AsRef<Path>) -> bool {
    let path = path.as_ref();
    path.exists()
}

pub fn create_new(path: impl AsRef<Path>, bytes: &[u8]) -> io::Result<()> {
    let path = path.as_ref();
    create_private_dir_all(parent(path))?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    sync_dir(parent(path))
}

pub fn write_database(path: impl AsRef<Path>, bytes: &[u8]) -> io::Result<()> {
    let path = path.as_ref();
    let mut file = File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    sync_dir(parent(path))
}

pub fn len(path: impl AsRef<Path>) -> io::Result<u64> {
    let path = path.as_ref();
    Ok(std::fs::metadata(path)?.len())
}

pub fn sync(path: impl AsRef<Path>) -> io::Result<bool> {
    let path = path.as_ref();
    match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => file.sync_all().map(|()| true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

pub async fn prepare(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        create_dir_all(parent)?;
    }
    Ok(())
}

fn lock_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

pub struct Owner(File);
impl Owner {
    pub async fn acquire(path: &Path) -> io::Result<Self> {
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path(path))?;
        lock.try_lock().map_err(|error| {
            io::Error::new(
                io::ErrorKind::ResourceBusy,
                format!("already open or file lock unavailable: {error}"),
            )
        })?;
        Ok(Self(lock))
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        let _ = self.0.unlock();
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    thread_local! {
        pub(super) static SYNCED: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
    }
    fn synced() -> Vec<PathBuf> {
        SYNCED.with(RefCell::take)
    }

    #[tokio::test]
    async fn new_and_renamed_files_sync_their_directories() -> io::Result<()> {
        let scratch = Scratch::new().await;
        let root = scratch.path();
        create_new(root.join("a/b/new"), b"new")?;
        assert_eq!(
            synced(),
            [root.to_path_buf(), root.join("a"), root.join("a/b")],
            "new directories sync their entries too, outermost first"
        );
        write_database(root.join("db"), b"db")?;
        assert_eq!(synced(), [root.to_path_buf()]);
        rename(root.join("a/b/new"), root.join("a/moved"))?;
        assert_eq!(synced(), [root.join("a"), root.join("a/b")]);
        write_private(root.join("private"), b"private")?;
        assert_eq!(synced(), [root.to_path_buf()]);
        assert_eq!(read(root.join("a/moved"))?, b"new");
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_dropped_owner_unlocks_while_a_spawned_process_still_holds_its_lock_descriptor()
    -> io::Result<()> {
        let scratch = Scratch::new().await;
        let path = scratch.path().join("owned.db");
        let owner = Owner::acquire(&path).await?;
        let mut inheritor = std::process::Command::new("cat")
            .stdin(std::process::Stdio::piped())
            .stdout(owner.0.try_clone()?)
            .spawn()?;
        drop(owner);
        let reopened = Owner::acquire(&path).await.map(drop);
        drop(inheritor.stdin.take());
        assert!(inheritor.wait()?.success());
        assert!(
            reopened.is_ok(),
            "a reopen must not wait for a process that inherited the lock descriptor: {reopened:?}"
        );
        Ok(())
    }
}
