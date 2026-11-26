use crate::{Error, sqlite};
use rocky_platform::fs;
use std::path::{Path, PathBuf};

pub struct DatabaseOwner {
    path: PathBuf,
    _owner: fs::Owner,
}
impl DatabaseOwner {
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn connect(&self) -> Result<sqlite::Connection, Error> {
        Ok(sqlite::Connection::open(&self.path)?)
    }
    pub async fn open(path: &Path) -> Result<(Self, sqlite::Connection), Error> {
        let path_string = path
            .to_str()
            .ok_or_else(|| Error::Incompatible("data path must be UTF-8".into()))?;
        if path_string == ":memory:" {
            return Err(Error::Incompatible("an on-disk file is required".into()));
        }
        fs::prepare(path).await.map_err(Error::Io)?;
        let wal_index = WalIndex::hold(path)?;
        check_file_format(path).await?;
        let owner = Self {
            path: path.to_path_buf(),
            _owner: fs::Owner::acquire(path).await.map_err(|error| {
                Error::Incompatible(format!(
                    "replica is already open or file lock unavailable: {error}"
                ))
            })?,
        };
        check_file_format(path).await?;
        let connection = owner.connect()?;
        if has_data(path) {
            connection.rows("SELECT name FROM sqlite_schema LIMIT 0", ())?;
        } else {
            connection.execute(INCREMENTAL_AUTO_VACUUM, ()).await?;
            connection
                .execute(format!("PRAGMA application_id={FILE_FORMAT}"), ())
                .await?;
        }
        drop(wal_index);
        Ok((owner, connection))
    }
}

pub struct WalIndex {
    _recovered: Option<sqlite::Connection>,
}
impl WalIndex {
    pub fn hold(path: &Path) -> Result<Self, Error> {
        let mut wal = path.as_os_str().to_owned();
        wal.push("-wal");
        let frames_to_recover = fs::len(Path::new(&wal)).is_ok_and(|len| len > 0);
        if !frames_to_recover || !has_data(path) {
            return Ok(Self { _recovered: None });
        }
        Ok(Self {
            _recovered: Some(sqlite::Connection::open_read_only(path)?),
        })
    }
}

const INCREMENTAL_AUTO_VACUUM: &str = "PRAGMA auto_vacuum=INCREMENTAL";

fn has_data(path: &Path) -> bool {
    fs::len(path).is_ok_and(|len| len > 0)
}

pub const FILE_FORMAT: i64 = 0x524b5903;

pub async fn check_file_format(path: &Path) -> Result<(), Error> {
    if !has_data(path) {
        return Ok(());
    }
    let conn = sqlite::Connection::open_read_only(path)?;
    let mut version = conn.query("PRAGMA application_id", ()).await?;
    if version
        .next()
        .await?
        .is_none_or(|row| row.get::<i64>(0).ok() != Some(FILE_FORMAT))
    {
        return Err(Error::Incompatible("unsupported Rocky file format".into()));
    }
    let mut tables = conn
        .query("SELECT name FROM sqlite_schema WHERE type='table'", ())
        .await?;
    while let Some(row) = tables.next().await? {
        let name = row.get::<String>(0)?;
        let scoped = name
            .strip_suffix("_rocky_logical_schema")
            .and_then(|prefix| prefix.strip_prefix('d'))
            .is_some_and(|hash| {
                hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
            });
        if name != "rocky_logical_schema" && !scoped {
            continue;
        }
        let quoted = name.replace('"', "\"\"");
        let mut descriptors = conn
            .query(
                format!("SELECT descriptor FROM \"{quoted}\" WHERE id=1"),
                (),
            )
            .await?;
        while let Some(row) = descriptors.next().await? {
            rocky_core::LogicalSchema::check_stored_row_identity(&row.get::<Vec<u8>>(0)?)
                .map_err(|error| Error::Incompatible(error.to_string()))?;
        }
    }
    Ok(())
}

#[cfg(all(test, not(target_family = "wasm")))]
mod tests {
    use super::*;
    fn lock_path(path: &Path) -> PathBuf {
        let mut name = path.as_os_str().to_owned();
        name.push(".lock");
        PathBuf::from(name)
    }
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn unsupported_files_are_refused_before_any_write() {
        for version in [0, FILE_FORMAT - 1, FILE_FORMAT + 1] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("file.sqlite");
            let conn = sqlite::Connection::open(&path).unwrap();
            conn.execute("CREATE TABLE sentinel(value TEXT)", ())
                .await
                .unwrap();
            conn.execute("INSERT INTO sentinel VALUES('retained')", ())
                .await
                .unwrap();
            conn.execute(format!("PRAGMA application_id={version}"), ())
                .await
                .unwrap();
            drop(conn);
            let before = std::fs::read(&path).unwrap();
            let error = DatabaseOwner::open(&path).await.err().unwrap();
            assert_eq!(
                error.to_string(),
                Error::Incompatible("unsupported Rocky file format".into()).to_string()
            );
            assert_eq!(std::fs::read(&path).unwrap(), before);
            assert!(!lock_path(&path).exists());
            assert!(!directory.path().join("file.sqlite-wal").exists());
        }
    }
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn unsupported_stored_descriptors_refuse_before_any_write() {
        for table in [
            "rocky_logical_schema",
            "d0000000000000000000000000000000000000000000000000000000000000000_rocky_logical_schema",
        ] {
            for version in [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 11] {
                let directory = tempfile::tempdir().unwrap();
                let path = directory.path().join("file.sqlite");
                let conn = sqlite::Connection::open(&path).unwrap();
                conn.execute(format!("PRAGMA application_id={FILE_FORMAT}"), ())
                    .await
                    .unwrap();
                conn.execute(
                    format!("CREATE TABLE {table}(id INTEGER PRIMARY KEY,descriptor BLOB)"),
                    (),
                )
                .await
                .unwrap();
                conn.execute(
                    format!("INSERT INTO {table} VALUES(1,?)"),
                    [format!("{{\"format\":{version}}}").into_bytes()],
                )
                .await
                .unwrap();
                drop(conn);
                let before = std::fs::read(&path).unwrap();
                let error = DatabaseOwner::open(&path).await.err().unwrap();
                assert_eq!(
                    error.to_string(),
                    Error::Incompatible(
                        "invalid logical schema: unsupported schema descriptor format".into()
                    )
                    .to_string()
                );
                assert_eq!(std::fs::read(&path).unwrap(), before);
                assert!(!lock_path(&path).exists());
                assert!(!directory.path().join("file.sqlite-wal").exists());
            }
        }
    }
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn application_table_suffixes_are_not_schema_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("application.sqlite");
        let (owner, conn) = DatabaseOwner::open(&path).await.unwrap();
        conn.execute("CREATE TABLE notes_rocky_logical_schema(body TEXT)", ())
            .await
            .unwrap();
        conn.execute(
            "INSERT INTO notes_rocky_logical_schema VALUES('retained')",
            (),
        )
        .await
        .unwrap();
        drop(conn);
        drop(owner);
        let before = std::fs::read(&path).unwrap();
        check_file_format(&path).await.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn an_owner_recovers_a_crashed_wal_once_across_its_checks() {
        crate::wal_recoveries::watch();
        let directory = tempfile::tempdir().unwrap();
        let (live, crashed) = (
            directory.path().join("live"),
            directory.path().join("crashed"),
        );
        std::fs::create_dir_all(&live).unwrap();
        std::fs::create_dir_all(&crashed).unwrap();
        let (owner, conn) = DatabaseOwner::open(&live.join("file.sqlite"))
            .await
            .unwrap();
        conn.run_now("PRAGMA journal_mode=wal").unwrap();
        conn.run_now("PRAGMA wal_autocheckpoint=0").unwrap();
        conn.run_now("CREATE TABLE sentinel(value TEXT)").unwrap();
        conn.run_now("INSERT INTO sentinel VALUES('retained')")
            .unwrap();
        for file in ["file.sqlite", "file.sqlite-wal"] {
            std::fs::copy(live.join(file), crashed.join(file)).unwrap();
        }
        drop((conn, owner));
        let path = crashed.join("file.sqlite");
        let (owner, conn) = DatabaseOwner::open(&path).await.unwrap();
        assert_eq!(
            crate::wal_recoveries::of(&path),
            1,
            "both format checks and the owner's connection share one recovery"
        );
        assert_eq!(
            conn.rows("SELECT value FROM sentinel", ()).unwrap(),
            vec![vec![sqlite::Value::Text("retained".into())]]
        );
        assert_eq!(crate::wal_recoveries::of(&path), 1);
        drop((conn, owner));
    }
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn a_new_file_reopens_with_the_current_tag() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("file.sqlite");
        let (owner, conn) = DatabaseOwner::open(&path).await.unwrap();
        conn.execute("CREATE TABLE sentinel(value TEXT)", ())
            .await
            .unwrap();
        drop(conn);
        drop(owner);
        check_file_format(&path).await.unwrap();
        let (owner, conn) = DatabaseOwner::open(&path).await.unwrap();
        drop(conn);
        drop(owner);
    }
}
