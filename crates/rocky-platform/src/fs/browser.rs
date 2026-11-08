//! Files in Rocky's OPFS pool. SQLite opens databases as pool entries
//! through the VFS. Every other file is a row of one WAL database in the
//! pool, so writes and renames are atomic and durable when they return,
//! like `write_private` natively. Directories are path prefixes.
use rocky_sqlite_opfs::sahpool::{self, OpfsSAHPoolCfg, OpfsSAHPoolUtil};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use std::{
    cell::{OnceCell, RefCell},
    collections::HashSet,
    io,
    path::{Component, Path},
};

/// The SQLite VFS that holds Rocky's files, and its OPFS directory.
pub const SQLITE_VFS: &str = "rocky-opfs";
const DIRECTORY: &str = ".rocky";
/// The database holding every file that is not itself a database.
const FILES: &str = ".rocky-files.sqlite";
/// Free pool entries kept for new files. Creating an entry is asynchronous
/// in OPFS, so each `prepare` tops the pool up.
const SPARE: u32 = 8;

thread_local! {
    static POOL: OnceCell<OpfsSAHPoolUtil> = const { OnceCell::new() };
    static TABLE: OnceCell<Connection> = const { OnceCell::new() };
    static OWNED: RefCell<HashSet<String>> = RefCell::default();
}

fn unavailable(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(format!(
        "browser storage (OPFS) is unavailable: {error}; Rocky must run in a dedicated Worker"
    ))
}
fn sqlite(error: rusqlite::Error) -> io::Error {
    io::Error::other(error)
}
fn not_ready() -> io::Error {
    io::Error::other("browser storage is not prepared; open the database first")
}
fn not_found(path: &str) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, format!("{path} does not exist"))
}

/// The pool's name for `path`: its normal components joined by `/`.
fn key(path: &Path) -> io::Result<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(
                part.to_str()
                    .ok_or_else(|| io::Error::other("data paths must be UTF-8"))?,
            ),
            Component::CurDir | Component::RootDir => {}
            Component::ParentDir | Component::Prefix(_) => {
                return Err(io::Error::other(format!(
                    "unsupported path {}",
                    path.display()
                )));
            }
        }
    }
    if parts.is_empty() {
        return Err(io::Error::other("an empty path names no file"));
    }
    Ok(parts.join("/"))
}

/// Unix microseconds, as files record when they were written.
fn now() -> i64 {
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_micros()).unwrap_or(i64::MAX)
        })
}

fn pool() -> io::Result<OpfsSAHPoolUtil> {
    POOL.with(|pool| pool.get().copied()).ok_or_else(not_ready)
}
fn table<T>(work: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> io::Result<T> {
    TABLE.with(|table| work(table.get().ok_or_else(not_ready)?).map_err(sqlite))
}
fn in_pool(key: &str) -> io::Result<bool> {
    pool()?.exists(key).map_err(unavailable)
}

/// Installs the pool and the file table and keeps room for new files.
/// Everything else here is synchronous, so a node prepares before it opens.
pub async fn prepare(path: &Path) -> io::Result<()> {
    key(path)?;
    let config = OpfsSAHPoolCfg {
        vfs_name: SQLITE_VFS.into(),
        directory: DIRECTORY.into(),
        clear_on_init: false,
        initial_capacity: 2 * SPARE,
    };
    let pool = sahpool::install::<rocky_sqlite::WasmOsCallback>(&config, false)
        .await
        .map_err(unavailable)?;
    POOL.with(|cell| cell.get_or_init(|| pool).count());
    pool.reserve_minimum_capacity(pool.count() + SPARE)
        .await
        .map_err(unavailable)?;
    if TABLE.with(|table| table.get().is_none()) {
        let connection =
            Connection::open_with_flags_and_vfs(FILES, OpenFlags::default(), SQLITE_VFS)
                .map_err(sqlite)?;
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
                 CREATE TABLE IF NOT EXISTS files(
                     path TEXT PRIMARY KEY,
                     bytes BLOB NOT NULL,
                     written INTEGER NOT NULL
                 ) WITHOUT ROWID;",
            )
            .map_err(sqlite)?;
        TABLE.with(|table| table.get_or_init(|| connection).is_autocommit());
    }
    Ok(())
}

pub fn read(path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
    let key = key(path.as_ref())?;
    if in_pool(&key)? {
        return pool()?.export_db(&key).map_err(unavailable);
    }
    table(|table| {
        table
            .query_row("SELECT bytes FROM files WHERE path=?", [&key], |row| {
                row.get(0)
            })
            .optional()
    })?
    .ok_or_else(|| not_found(&key))
}

pub fn read_to_string(path: impl AsRef<Path>) -> io::Result<String> {
    String::from_utf8(read(path)?)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

pub fn write(path: impl AsRef<Path>, bytes: impl AsRef<[u8]>) -> io::Result<()> {
    let key = key(path.as_ref())?;
    if in_pool(&key)? {
        return Err(io::Error::other(format!("{key} is a database")));
    }
    table(|table| {
        table
            .execute(
                "INSERT INTO files VALUES(?1,?2,?3)
                 ON CONFLICT(path) DO UPDATE SET bytes=?2, written=?3",
                params![key, bytes.as_ref(), now()],
            )
            .map(drop)
    })
}

/// Replaces `path` atomically, durable on return. OPFS is private to the
/// origin, so there is no owner-only mode.
pub fn write_private(path: impl AsRef<Path>, bytes: &[u8]) -> io::Result<()> {
    let path = path.as_ref();
    write(path, bytes)
}

/// Writes a new file, durable on return; an existing one fails with
/// `AlreadyExists`.
pub fn create_new(path: impl AsRef<Path>, bytes: &[u8]) -> io::Result<()> {
    let path = path.as_ref();
    let key = key(path)?;
    let exists = || io::Error::new(io::ErrorKind::AlreadyExists, format!("{key} exists"));
    if in_pool(&key)? {
        return Err(exists());
    }
    match table(|table| {
        table.execute(
            "INSERT INTO files VALUES(?1,?2,?3) ON CONFLICT(path) DO NOTHING",
            params![key, bytes, now()],
        )
    })? {
        0 => Err(exists()),
        _ => Ok(()),
    }
}

/// Replaces the database file at `path` with `bytes`, durable on return:
/// a pool entry, where SQLite opens it.
pub fn write_database(path: impl AsRef<Path>, bytes: &[u8]) -> io::Result<()> {
    let key = key(path.as_ref())?;
    table(|table| {
        table
            .execute("DELETE FROM files WHERE path=?", [&key])
            .map(drop)
    })?;
    let pool = pool()?;
    pool.delete_db(&key).map_err(unavailable)?;
    pool.write(&key, bytes).map_err(unavailable)
}

pub fn remove_file(path: impl AsRef<Path>) -> io::Result<()> {
    let key = key(path.as_ref())?;
    if in_pool(&key)? {
        return pool()?.delete_db(&key).map(drop).map_err(unavailable);
    }
    match table(|table| table.execute("DELETE FROM files WHERE path=?", [&key]))? {
        0 => Err(not_found(&key)),
        _ => Ok(()),
    }
}

pub fn remove_dir_all(path: impl AsRef<Path>) -> io::Result<()> {
    let prefix = format!("{}/", key(path.as_ref())?);
    let pool = pool()?;
    for name in pool
        .list()
        .into_iter()
        .filter(|name| name.starts_with(&prefix))
    {
        pool.delete_db(&name).map_err(unavailable)?;
    }
    table(|table| {
        table
            .execute(
                "DELETE FROM files WHERE substr(path,1,length(?1))=?1",
                [&prefix],
            )
            .map(drop)
    })
}

/// Every change is durable when it returns; there is no directory to sync.
pub fn sync_dir(directory: impl AsRef<Path>) -> io::Result<()> {
    key(directory.as_ref()).map(drop)
}

/// Directories are prefixes; there is nothing to create.
pub fn create_dir_all(path: impl AsRef<Path>) -> io::Result<()> {
    key(path.as_ref()).map(drop)
}
pub fn create_private_dir_all(path: impl AsRef<Path>) -> io::Result<()> {
    let path = path.as_ref();
    create_dir_all(path)
}

pub fn rename(from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<()> {
    let (from, to) = (key(from.as_ref())?, key(to.as_ref())?);
    if in_pool(&from)? {
        return pool()?.rename(&from, &to).map_err(unavailable);
    }
    let renamed = table(|table| {
        let transaction = table.unchecked_transaction()?;
        transaction.execute("DELETE FROM files WHERE path=?", [&to])?;
        let renamed =
            transaction.execute("UPDATE files SET path=?2 WHERE path=?1", [&from, &to])?;
        transaction.commit()?;
        Ok(renamed)
    })?;
    if renamed == 0 {
        return Err(not_found(&from));
    }
    Ok(())
}

pub fn copy(from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<u64> {
    let (from, to) = (key(from.as_ref())?, key(to.as_ref())?);
    if in_pool(&from)? {
        let pool = pool()?;
        let bytes = pool.export_db(&from).map_err(unavailable)?;
        pool.delete_db(&to).map_err(unavailable)?;
        pool.write(&to, &bytes).map_err(unavailable)?;
        return Ok(bytes.len() as u64);
    }
    let bytes = read(&from)?;
    write(&to, &bytes)?;
    Ok(bytes.len() as u64)
}

/// Whether a file exists at `path`, or any file under it.
pub fn exists(path: impl AsRef<Path>) -> bool {
    let path = path.as_ref();
    let Ok(key) = key(path) else {
        return false;
    };
    let prefix = format!("{key}/");
    let in_pool = pool().is_ok_and(|pool| {
        pool.list()
            .iter()
            .any(|name| *name == key || name.starts_with(&prefix))
    });
    in_pool
        || table(|table| {
            table
                .query_row(
                    "SELECT 1 FROM files WHERE path=?1 OR substr(path,1,length(?2))=?2 LIMIT 1",
                    [&key, &prefix],
                    |_| Ok(()),
                )
                .optional()
        })
        .is_ok_and(|row| row.is_some())
}

pub fn len(path: impl AsRef<Path>) -> io::Result<u64> {
    let path = path.as_ref();
    let key = key(path)?;
    if let Some(size) = pool()?.size(&key).map_err(unavailable)? {
        return Ok(size);
    }
    table(|table| {
        table
            .query_row(
                "SELECT length(bytes) FROM files WHERE path=?",
                [&key],
                |row| row.get::<_, i64>(0),
            )
            .optional()
    })?
    .map(i64::unsigned_abs)
    .ok_or_else(|| not_found(&key))
}

/// The paths of the files and prefixes directly in `directory`.
pub fn read_dir(directory: impl AsRef<Path>) -> io::Result<Vec<std::path::PathBuf>> {
    let directory = directory.as_ref();
    let prefix = format!("{}/", key(directory)?);
    let mut names: Vec<String> = pool()?.list();
    names.extend(table(|table| {
        table
            .prepare("SELECT path FROM files WHERE substr(path,1,length(?1))=?1")?
            .query_map([&prefix], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()
    })?);
    let mut entries: Vec<_> = names
        .iter()
        .filter_map(|name| name.strip_prefix(&prefix))
        .filter_map(|rest| rest.split('/').next())
        .map(|child| directory.join(child))
        .collect();
    entries.sort();
    entries.dedup();
    Ok(entries)
}

/// When the file at `path` was last written. Databases, which SQLite writes,
/// report no time.
pub fn modified(path: impl AsRef<Path>) -> io::Result<web_time::SystemTime> {
    let path = path.as_ref();
    let key = key(path)?;
    let written: Option<i64> = table(|table| {
        table
            .query_row("SELECT written FROM files WHERE path=?", [&key], |row| {
                row.get(0)
            })
            .optional()
    })?;
    let written = written.ok_or_else(|| not_found(&key))?;
    Ok(web_time::UNIX_EPOCH + std::time::Duration::from_micros(written.unsigned_abs()))
}

/// Makes the file at `path` durable; `false` if there is none.
pub fn sync(path: impl AsRef<Path>) -> io::Result<bool> {
    let path = path.as_ref();
    let key = key(path)?;
    if in_pool(&key)? {
        return pool()?.flush(&key).map(|()| true).map_err(unavailable);
    }
    // Rows commit at `synchronous=FULL`, so they are durable already.
    table(|table| {
        table
            .query_row("SELECT 1 FROM files WHERE path=?", [&key], |_| Ok(()))
            .optional()
    })
    .map(|row| row.is_some())
}

/// One owner per file in this worker; the pool excludes other workers.
pub struct Owner(String);
impl Owner {
    pub async fn acquire(path: &Path) -> io::Result<Self> {
        let key = key(path)?;
        if OWNED.with(|owned| owned.borrow_mut().insert(key.clone())) {
            Ok(Self(key))
        } else {
            Err(io::Error::new(io::ErrorKind::ResourceBusy, "already open"))
        }
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        OWNED.with(|owned| owned.borrow_mut().remove(&self.0));
    }
}

#[cfg(feature = "test-support")]
pub struct Scratch(std::path::PathBuf);
#[cfg(feature = "test-support")]
impl Scratch {
    pub async fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory =
            std::path::PathBuf::from(format!("scratch/{}", NEXT.fetch_add(1, Ordering::Relaxed)));
        prepare(&directory.join("scratch"))
            .await
            .expect("browser storage");
        Self(directory)
    }
    pub fn path(&self) -> &Path {
        &self.0
    }
}
