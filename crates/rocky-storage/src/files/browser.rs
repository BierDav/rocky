use crate::Error;
use sqlite_wasm_vfs::sahpool::{self, OpfsSAHPoolCfg, OpfsSAHPoolUtil};
use std::{
    cell::{OnceCell, RefCell},
    collections::HashSet,
    path::{Path, PathBuf},
};

/// The SQLite VFS that holds Rocky's files, and its OPFS directory.
const VFS: &str = "rocky-opfs";
const DIRECTORY: &str = ".rocky";
/// Free pool entries kept for new files: a database, its WAL and journal.
const SPARE: u32 = 4;

thread_local! {
    static POOL: OnceCell<OpfsSAHPoolUtil> = const { OnceCell::new() };
    static OWNED: RefCell<HashSet<PathBuf>> = RefCell::default();
}

fn unavailable(error: impl std::fmt::Display) -> Error {
    Error::Incompatible(format!(
        "browser storage (OPFS) is unavailable: {error}; Rocky must run in a dedicated Worker"
    ))
}

/// Installs the OPFS pool and keeps room in it for `path`. Opening a file
/// is synchronous, so the pool must be ready before anything connects.
pub async fn prepare(path: &Path) -> Result<(), Error> {
    path.to_str()
        .ok_or_else(|| Error::Incompatible("data path must be UTF-8".into()))?;
    let config = OpfsSAHPoolCfg {
        vfs_name: VFS.into(),
        directory: DIRECTORY.into(),
        clear_on_init: false,
        initial_capacity: 2 * SPARE,
    };
    let pool = sahpool::install::<sqlite_wasm_rs::WasmOsCallback>(&config, false)
        .await
        .map_err(unavailable)?;
    POOL.with(|cell| cell.get_or_init(|| pool).count());
    pool.reserve_minimum_capacity(pool.count() + SPARE)
        .await
        .map_err(unavailable)
}

/// Whether `path` exists in the pool and is not empty.
pub fn has_data(path: &Path) -> bool {
    POOL.with(|pool| {
        pool.get()
            .zip(path.to_str())
            .and_then(|(pool, name)| pool.size(name).ok().flatten())
            .is_some_and(|size| size > 0)
    })
}

pub(crate) fn connect(
    path: &Path,
    flags: rusqlite::OpenFlags,
) -> rusqlite::Result<rusqlite::Connection> {
    rusqlite::Connection::open_with_flags_and_vfs(path, flags, VFS)
}

/// One owner per file in this worker; the pool excludes other workers.
pub struct Owner(PathBuf);
impl Owner {
    pub async fn acquire(path: &Path) -> Result<Self, Error> {
        if OWNED.with(|owned| owned.borrow_mut().insert(path.to_path_buf())) {
            Ok(Self(path.to_path_buf()))
        } else {
            Err(Error::Incompatible("replica is already open".into()))
        }
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        OWNED.with(|owned| owned.borrow_mut().remove(&self.0));
    }
}

#[cfg(any(test, feature = "test-support"))]
pub struct Scratch(PathBuf);
#[cfg(any(test, feature = "test-support"))]
impl Scratch {
    pub async fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = PathBuf::from(format!("scratch/{}", NEXT.fetch_add(1, Ordering::Relaxed)));
        prepare(&directory.join("scratch"))
            .await
            .expect("the OPFS pool");
        Self(directory)
    }
    pub fn path(&self) -> &Path {
        &self.0
    }
}
