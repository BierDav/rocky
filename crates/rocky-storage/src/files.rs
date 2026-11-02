//! Where database files live. Natively a path names a file, owned through a
//! `.lock` beside it. In a browser (wasm32) it names a file in Rocky's OPFS
//! pool: only a dedicated Worker can open the pool, and its sync access
//! handles exclude every other worker and tab. Everything else is shared.
#[cfg_attr(not(target_family = "wasm"), path = "files/native.rs")]
#[cfg_attr(target_family = "wasm", path = "files/browser.rs")]
mod platform;

pub(crate) use platform::connect;
pub use platform::{Owner, has_data, prepare};

/// A fresh directory for tests: a temporary directory natively, a new name
/// in the pool in a browser, where every test run starts on an empty OPFS.
#[cfg(any(test, feature = "test-support"))]
pub use platform::Scratch;

// The same SQLite locking and WAL behavior on every target.
#[cfg(test)]
mod tests {
    use super::Scratch;
    use crate::{sqlite::Connection, storage::collect_rows};
    use rocky_core::SqlValue;

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
        assert!(super::has_data(&path));
    }
}
