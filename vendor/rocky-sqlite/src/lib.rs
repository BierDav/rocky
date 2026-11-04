//! Rocky's SQLite. Natively libsqlite3-sys compiles it; in a browser this
//! crate does, with the same source and flags (see `build.rs`), and rusqlite
//! links it through libsqlite3-sys without `bundled`.
//!
//! The preupdate hook lives here because rusqlite's `preupdate_hook` feature
//! runs bindgen against a system `sqlite3.h`, which a browser build lacks.
#![allow(clippy::missing_safety_doc)]
#![cfg_attr(target_feature = "atomics", feature(stdarch_wasm_atomic_wait))]

#[cfg(all(target_family = "wasm", target_os = "unknown"))]
extern crate alloc;

#[cfg(all(target_family = "wasm", target_os = "unknown"))]
#[allow(non_camel_case_types)]
mod shim;

#[cfg(all(target_family = "wasm", target_os = "unknown"))]
pub use shim::WasmOsCallback;

use std::ffi::{CStr, c_char, c_int, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};

use rusqlite::ffi;
use rusqlite::types::ValueRef;

/// How a row changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Insert,
    Delete,
    Update,
}

/// A row change SQLite is about to make, valid during the hook call.
pub struct PreUpdate {
    db: *mut ffi::sqlite3,
    change: Change,
}

impl PreUpdate {
    pub fn change(&self) -> Change {
        self.change
    }

    /// The changed table's column count.
    pub fn columns(&self) -> i32 {
        // SAFETY: `db` is the connection running this hook.
        unsafe { ffi::sqlite3_preupdate_count(self.db) }
    }

    /// 0 for a statement's own change, 1 for a change of its triggers, and
    /// so on.
    pub fn depth(&self) -> i32 {
        // SAFETY: `db` is the connection running this hook.
        unsafe { ffi::sqlite3_preupdate_depth(self.db) }
    }

    /// A column before a delete or update.
    pub fn before(&self, column: i32) -> rusqlite::Result<ValueRef<'_>> {
        let mut value = std::ptr::null_mut();
        // SAFETY: `db` is the connection running this hook; SQLite refuses
        // an insert and a column out of range.
        check(unsafe { ffi::sqlite3_preupdate_old(self.db, column, &mut value) })?;
        // SAFETY: SQLite returned the value, valid for this call.
        unsafe { value_ref(value) }
    }

    /// A column after an insert or update.
    pub fn after(&self, column: i32) -> rusqlite::Result<ValueRef<'_>> {
        let mut value = std::ptr::null_mut();
        // SAFETY: as in `before`.
        check(unsafe { ffi::sqlite3_preupdate_new(self.db, column, &mut value) })?;
        // SAFETY: as in `before`.
        unsafe { value_ref(value) }
    }
}

/// Calls `hook(database, table, row)` before each row change of
/// `connection`, replacing an earlier hook. The connection frees the hook
/// when it closes.
pub fn preupdate_hook<F>(connection: &rusqlite::Connection, hook: F) -> rusqlite::Result<()>
where
    F: FnMut(&str, &str, &PreUpdate) + Send + 'static,
{
    unsafe extern "C" fn call<F: FnMut(&str, &str, &PreUpdate)>(
        data: *mut c_void,
        db: *mut ffi::sqlite3,
        op: c_int,
        database: *const c_char,
        table: *const c_char,
        _old_row: i64,
        _new_row: i64,
    ) {
        let change = match op {
            ffi::SQLITE_INSERT => Change::Insert,
            ffi::SQLITE_DELETE => Change::Delete,
            ffi::SQLITE_UPDATE => Change::Update,
            _ => return,
        };
        // SAFETY: `data` is the `F` registered below, which lives until the
        // connection closes; the names are SQLite's nul-terminated strings.
        let (hook, database, table) = unsafe {
            (
                &mut *data.cast::<F>(),
                CStr::from_ptr(database).to_string_lossy(),
                CStr::from_ptr(table).to_string_lossy(),
            )
        };
        let row = PreUpdate { db, change };
        // A panic must not unwind into SQLite.
        let _ = catch_unwind(AssertUnwindSafe(|| hook(&database, &table, &row)));
    }
    unsafe extern "C" fn free<F>(data: *mut c_void) {
        // SAFETY: `data` came from `Box::into_raw` below, freed only here.
        drop(unsafe { Box::from_raw(data.cast::<F>()) });
    }

    // SAFETY: the handle is not closed or shared beyond these calls.
    let db = unsafe { connection.handle() };
    let data = Box::into_raw(Box::new(hook)).cast::<c_void>();
    // SAFETY: the hook points at `data` before the client data replaces, and
    // so frees, an earlier hook's. On failure SQLite has already freed
    // `data`, so the hook is removed.
    unsafe {
        ffi::sqlite3_preupdate_hook(db, Some(call::<F>), data);
        let code = ffi::sqlite3_set_clientdata(db, HOOK.as_ptr(), data, Some(free::<F>));
        if code != ffi::SQLITE_OK {
            ffi::sqlite3_preupdate_hook(db, None, std::ptr::null_mut());
        }
        check(code)
    }
}

const HOOK: &CStr = c"rocky_sqlite.preupdate";

fn check(code: c_int) -> rusqlite::Result<()> {
    if code == ffi::SQLITE_OK {
        Ok(())
    } else {
        Err(rusqlite::Error::SqliteFailure(ffi::Error::new(code), None))
    }
}

/// # Safety
/// `value` is a protected SQLite value that outlives `'a`.
unsafe fn value_ref<'a>(value: *mut ffi::sqlite3_value) -> rusqlite::Result<ValueRef<'a>> {
    let bytes = |pointer: *const c_void| {
        // SAFETY: SQLite returns `bytes` bytes at `pointer`, or no pointer
        // for an empty value or when it cannot allocate.
        let length = usize::try_from(unsafe { ffi::sqlite3_value_bytes(value) }).unwrap_or(0);
        match (pointer.is_null(), length) {
            (_, 0) => Ok(&[][..]),
            (true, _) => Err(rusqlite::Error::SqliteFailure(
                ffi::Error::new(ffi::SQLITE_NOMEM),
                None,
            )),
            (false, length) => Ok(unsafe { std::slice::from_raw_parts(pointer.cast(), length) }),
        }
    };
    // SAFETY: `value` is valid, per the caller.
    unsafe {
        Ok(match ffi::sqlite3_value_type(value) {
            ffi::SQLITE_INTEGER => ValueRef::Integer(ffi::sqlite3_value_int64(value)),
            ffi::SQLITE_FLOAT => ValueRef::Real(ffi::sqlite3_value_double(value)),
            ffi::SQLITE_TEXT => ValueRef::Text(bytes(ffi::sqlite3_value_text(value).cast())?),
            ffi::SQLITE_BLOB => ValueRef::Blob(bytes(ffi::sqlite3_value_blob(value))?),
            _ => ValueRef::Null,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    type Seen = Vec<(String, Change, i32, Vec<String>)>;

    fn text(value: rusqlite::Result<ValueRef<'_>>) -> String {
        match value {
            Ok(ValueRef::Integer(value)) => value.to_string(),
            Ok(ValueRef::Text(value)) => String::from_utf8_lossy(value).into(),
            Ok(ValueRef::Null) => "null".into(),
            Ok(other) => format!("{other:?}"),
            Err(_) => "error".into(),
        }
    }

    #[cfg_attr(
        all(target_family = "wasm", target_os = "unknown"),
        wasm_bindgen_test::wasm_bindgen_test
    )]
    #[cfg_attr(not(all(target_family = "wasm", target_os = "unknown")), test)]
    fn preupdate_reports_rows_and_depth_and_is_replaced() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE rows(id INTEGER PRIMARY KEY, value TEXT);
                 CREATE TABLE copies(id INTEGER PRIMARY KEY);
                 CREATE TRIGGER copy AFTER INSERT ON rows BEGIN INSERT INTO copies VALUES(NEW.id); END;",
            )
            .unwrap();
        let seen = Arc::new(Mutex::new(Seen::new()));
        let replaced = Arc::new(Mutex::new(Seen::new()));
        let record = |seen: &Arc<Mutex<Seen>>| {
            let seen = seen.clone();
            move |database: &str, table: &str, row: &PreUpdate| {
                assert_eq!(database, "main");
                let values = (0..row.columns())
                    .map(|column| {
                        format!("{}>{}", text(row.before(column)), text(row.after(column)))
                    })
                    .collect();
                seen.lock()
                    .unwrap()
                    .push((table.into(), row.change(), row.depth(), values));
            }
        };
        preupdate_hook(&connection, record(&replaced)).unwrap();
        preupdate_hook(&connection, record(&seen)).unwrap();
        connection
            .execute_batch(
                "INSERT INTO rows VALUES(1, 'a'); UPDATE rows SET value = x'00' WHERE id = 1;
                 DELETE FROM rows;",
            )
            .unwrap();
        assert!(replaced.lock().unwrap().is_empty());
        let row = |table: &str, change, depth, values: &[&str]| {
            let values = values.iter().map(|value| value.to_string()).collect();
            (table.to_string(), change, depth, values)
        };
        assert_eq!(
            *seen.lock().unwrap(),
            [
                row("rows", Change::Insert, 0, &["error>1", "error>a"]),
                row("copies", Change::Insert, 1, &["error>1"]),
                row("rows", Change::Update, 0, &["1>1", "a>Blob([0])"]),
                row("rows", Change::Delete, 0, &["1>error", "Blob([0])>error"]),
            ]
        );
        drop(connection);
        assert_eq!(Arc::strong_count(&seen), 1, "closing frees the hook");
    }
}
