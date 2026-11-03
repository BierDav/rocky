//! How SQLite opens database files: through the filesystem natively, and
//! through Rocky's OPFS pool in a browser (`rocky_platform::fs`).
use std::path::Path;

/// A raw engine connection to `path` for tests, through the same VFS.
#[cfg(any(test, feature = "test-support"))]
pub fn raw(path: &Path) -> rusqlite::Result<rusqlite::Connection> {
    connect(path, rusqlite::OpenFlags::default())
}

pub(crate) fn connect(
    path: &Path,
    flags: rusqlite::OpenFlags,
) -> rusqlite::Result<rusqlite::Connection> {
    #[cfg(not(target_family = "wasm"))]
    return rusqlite::Connection::open_with_flags(path, flags);
    #[cfg(target_family = "wasm")]
    return rusqlite::Connection::open_with_flags_and_vfs(
        path,
        flags,
        rocky_platform::fs::SQLITE_VFS,
    );
}

// The same SQLite locking and WAL behavior on every target.
#[cfg(test)]
mod tests {
    use crate::{sqlite::Connection, storage::collect_rows};
    use rocky_core::SqlValue;
    use rocky_platform::fs::Scratch;

    async fn rows(connection: &Connection, sql: &str) -> Vec<Vec<SqlValue>> {
        collect_rows(connection.query(sql, ()).await.unwrap())
            .await
            .unwrap()
    }
    async fn count(connection: &Connection) -> i64 {
        match rows(connection, "SELECT count(*) FROM t").await[..] {
            [ref row] => match row[..] {
                [SqlValue::Integer(count)] => count,
                _ => panic!("not a count"),
            },
            _ => panic!("not one row"),
        }
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn wal_readers_keep_their_snapshot_while_writers_exclude_each_other() {
        let directory = Scratch::new().await;
        let path = directory.path().join("wal.sqlite");
        let writer = Connection::open(&path).unwrap();
        assert_eq!(
            rows(&writer, "PRAGMA journal_mode=wal").await,
            vec![vec![SqlValue::Text("wal".into())]]
        );
        writer.run_now("CREATE TABLE t(x)").unwrap();
        writer.run_now("INSERT INTO t VALUES(1)").unwrap();

        let reader = Connection::open_read_only(&path).unwrap();
        reader.run_now("BEGIN").unwrap();
        assert_eq!(count(&reader).await, 1);
        writer.run_now("INSERT INTO t VALUES(2)").unwrap();
        assert_eq!(count(&reader).await, 1, "the reader keeps its snapshot");
        reader.run_now("COMMIT").unwrap();
        assert_eq!(count(&reader).await, 2);

        let other = Connection::open(&path).unwrap();
        writer.run_now("BEGIN IMMEDIATE").unwrap();
        assert!(
            other.run_now("BEGIN IMMEDIATE").is_err(),
            "one writer at a time"
        );
        writer.run_now("COMMIT").unwrap();
        other.run_now("BEGIN IMMEDIATE").unwrap();
        other.run_now("INSERT INTO t VALUES(3)").unwrap();
        other.run_now("COMMIT").unwrap();
        assert_eq!(count(&writer).await, 3);

        // The last connection checkpoints; a new one rebuilds the wal-index.
        drop((writer, reader, other));
        let reopened = Connection::open_read_only(&path).unwrap();
        assert_eq!(count(&reopened).await, 3);
        assert!(rocky_platform::fs::len(&path).unwrap() > 0);
    }
}
