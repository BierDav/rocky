use rusqlite::ffi;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::path::Path;
use std::sync::{Mutex, Once, PoisonError};

static RECOVERED: Mutex<Vec<String>> = Mutex::new(Vec::new());
static LOGGING: Once = Once::new();
const RECOVERED_FROM: &str = " frames from WAL file ";

extern "C" fn logged(_unused: *mut c_void, code: c_int, message: *const c_char) {
    if code != ffi::SQLITE_NOTICE_RECOVER_WAL || message.is_null() {
        return;
    }
    let message = unsafe { CStr::from_ptr(message) }.to_string_lossy();
    if let Some((_, wal)) = message.split_once(RECOVERED_FROM) {
        RECOVERED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(wal.to_owned());
    }
}

pub fn watch() {
    LOGGING.call_once(|| {
        let configured = unsafe {
            ffi::sqlite3_config(
                ffi::SQLITE_CONFIG_LOG,
                logged as extern "C" fn(*mut c_void, c_int, *const c_char),
                std::ptr::null_mut::<c_void>(),
            )
        };
        assert_eq!(
            configured,
            ffi::SQLITE_OK,
            "SQLite accepts a log callback after initialization"
        );
    });
}

pub fn of(database: &Path) -> usize {
    let wal = |path: &Path| {
        let file = path.file_name()?.to_string_lossy();
        let directory = path.parent()?.file_name()?.to_string_lossy();
        Some(format!("/{directory}/{file}-wal"))
    };
    let Some(wal) = wal(database) else {
        return 0;
    };
    RECOVERED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .filter(|recovered| recovered.ends_with(&wal))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reopening_a_crashed_wal_recovers_it_once_per_cold_open() {
        watch();
        let directory =
            std::env::temp_dir().join(format!("rocky-wal-recoveries-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("crashed.sqlite");
        let crashed = directory.join("copy.sqlite");
        let writer = rusqlite::Connection::open(&path).unwrap();
        writer
            .execute_batch(
                "PRAGMA journal_mode=wal; PRAGMA wal_autocheckpoint=0; CREATE TABLE t(x); INSERT INTO t VALUES(1)",
            )
            .unwrap();
        std::fs::copy(&path, &crashed).unwrap();
        std::fs::copy(
            directory.join("crashed.sqlite-wal"),
            directory.join("copy.sqlite-wal"),
        )
        .unwrap();
        drop(writer);
        let count = |connection: &rusqlite::Connection| {
            connection
                .query_row("SELECT count(*) FROM t", [], |row| row.get::<_, i64>(0))
                .unwrap()
        };
        let first = rusqlite::Connection::open(&crashed).unwrap();
        assert_eq!(count(&first), 1);
        assert_eq!(of(&crashed), 1);
        let shared = rusqlite::Connection::open(&crashed).unwrap();
        assert_eq!(count(&shared), 1);
        assert_eq!(of(&crashed), 1, "a second connection shares the wal-index");
        drop((first, shared));
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
