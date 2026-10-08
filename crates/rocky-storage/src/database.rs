use crate::{Error, sqlite};
use std::{
    fs::{File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
};

/// File lifetime is separate from scoped reducer/author/revision scheduling.
/// `<file>.lock` beside the file: appended, so `a.db` and `a.sqlite` in one
/// directory do not share a lock.
fn lock_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}
/// The lock file excludes a second process, so one writer owns the file.
pub struct DatabaseOwner {
    path: PathBuf,
    _lock: File,
}
impl DatabaseOwner {
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn connect(&self) -> Result<sqlite::Connection, Error> {
        Ok(sqlite::Connection::open(&self.path)?)
    }
    pub fn connect_read_only(&self) -> Result<sqlite::Connection, Error> {
        Ok(sqlite::Connection::open_read_only(&self.path)?)
    }
    pub async fn open(path: &Path) -> Result<(Self, sqlite::Connection), Error> {
        let path_string = path
            .to_str()
            .ok_or_else(|| Error::Incompatible("data path must be UTF-8".into()))?;
        if path_string == ":memory:" {
            return Err(Error::Incompatible("an on-disk file is required".into()));
        }
        refuse_foreign_engine(path)?;
        refuse_old_row_identity(path).await?;
        if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(Error::Io)?;
        }
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
        refuse_foreign_engine(path)?;
        let owner = Self {
            path: path.to_path_buf(),
            _lock: lock,
        };
        refuse_old_row_identity(path).await?;
        let connection = owner.connect()?;
        Ok((owner, connection))
    }
}

/// Files written by the earlier MVCC engine keep committed rows in a separate
/// logical log, so SQLite would see them as unreadable or empty. Refuse them.
fn refuse_foreign_engine(path: &Path) -> Result<(), Error> {
    let refused = || {
        Error::Incompatible(
            "file was written by the earlier MVCC storage engine; export it with the build \
             that wrote it and import into a new file"
                .into(),
        )
    };
    let mut log = path.as_os_str().to_owned();
    log.push("-log");
    if std::fs::metadata(&log).is_ok_and(|log| log.len() > 0) {
        return Err(refused());
    }
    let mut header = [0u8; 20];
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(Error::Io(error)),
    };
    match file.read_exact(&mut header) {
        Ok(()) => {}
        // A file shorter than its header is new or empty; SQLite decides.
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
        Err(error) => return Err(Error::Io(error)),
    }
    // Header bytes 18 and 19 are the write and read format: 1 rollback, 2 WAL.
    if header.starts_with(b"SQLite format 3\0") && (header[18] > 2 || header[19] > 2) {
        return Err(refused());
    }
    Ok(())
}

/// Read-only preflight, also used before SDK settings/enrollment writes.
pub async fn refuse_old_row_identity(path: &Path) -> Result<(), Error> {
    if !std::fs::metadata(path).is_ok_and(|file| file.len() > 0) {
        return Ok(());
    }
    let conn = sqlite::Connection::open_read_only(path)?;
    let mut rows = conn
        .query("SELECT name FROM sqlite_schema WHERE type='table'", ())
        .await?;
    let mut tables = Vec::new();
    while let Some(row) = rows.next().await? {
        tables.push(row.get::<String>(0)?);
    }
    let encrypted = tables.iter().any(|name| name == "secure_metadata");
    for name in tables {
        let scoped = name
            .strip_suffix("_rocky_logical_schema")
            .and_then(|prefix| prefix.strip_prefix('d'))
            .is_some_and(|hash| {
                hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
            });
        if !(encrypted && name == "secure_typed") && name != "rocky_logical_schema" && !scoped {
            continue;
        }
        let quoted = name.replace('"', "\"\"");
        let mut descriptors = conn
            .query(format!("SELECT descriptor FROM \"{quoted}\""), ())
            .await?;
        while let Some(row) = descriptors.next().await? {
            rocky_core::LogicalSchema::check_stored_row_identity(&row.get::<Vec<u8>>(0)?)
                .map_err(|error| Error::Incompatible(error.to_string()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod identity_tests {
    use super::*;

    #[tokio::test]
    async fn application_table_suffixes_are_not_schema_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("application.sqlite");
        let conn = sqlite::Connection::open(&path).unwrap();
        conn.execute("CREATE TABLE notes_rocky_logical_schema(body TEXT)", ())
            .await
            .unwrap();
        conn.execute(
            "INSERT INTO notes_rocky_logical_schema VALUES('application data')",
            (),
        )
        .await
        .unwrap();
        drop(conn);
        let before = std::fs::read(&path).unwrap();
        refuse_old_row_identity(&path).await.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!lock_path(&path).exists());
    }

    #[tokio::test]
    async fn old_and_unknown_typed_files_are_refused_without_writes() {
        for table in [
            "secure_typed",
            "rocky_logical_schema",
            "daaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa_rocky_logical_schema",
        ] {
            for format in [1, 0, 4] {
                let directory = tempfile::tempdir().unwrap();
                let path = directory.path().join("old.sqlite");
                let conn = sqlite::Connection::open(&path).unwrap();
                conn.execute(
                    format!("CREATE TABLE {table}(descriptor BLOB NOT NULL)"),
                    (),
                )
                .await
                .unwrap();
                if table == "secure_typed" {
                    conn.execute("CREATE TABLE secure_metadata(identity BLOB)", ())
                        .await
                        .unwrap();
                }
                let bytes = format!("{{\"format\":{format}}}").into_bytes();
                conn.execute(format!("INSERT INTO {table} VALUES(?)"), [bytes])
                    .await
                    .unwrap();
                conn.execute("CREATE TABLE sentinel(value TEXT)", ())
                    .await
                    .unwrap();
                conn.execute("INSERT INTO sentinel VALUES('retained')", ())
                    .await
                    .unwrap();
                drop(conn);
                let before = std::fs::read(&path).unwrap();
                let error = DatabaseOwner::open(&path).await.err().unwrap().to_string();
                assert!(error.contains("row identity"), "{error}");
                assert!(error.contains("export"), "{error}");
                assert!(error.contains("reinstall"), "{error}");
                assert_eq!(std::fs::read(&path).unwrap(), before);
                assert!(!path.with_extension("sqlite-wal").exists());
                assert!(!lock_path(&path).exists());
            }
        }
    }
}
