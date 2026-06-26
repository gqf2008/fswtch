//! Safe wrapper over FreeSWITCH's built-in SQLite (the `switch_core_db_*` family).
//!
//! This module mirrors the small `sqlite3-rs` ergonomics surface: an owned [`CoreDb`] connection
//! and borrowed [`Stmt`] prepared statements whose lifetime is tied to the connection. The
//! underlying FFI returns SQLite result codes (`SWITCH_CORE_DB_*`, plain `c_int`) rather than
//! `switch_status_t`, so these wrappers translate non-OK codes into [`crate::SwitchError`]
//! (carrying [`crate::GENERR`]) and let callers recover the engine's own message via
//! [`CoreDb::errmsg`].
use std::ffi::CString;
use std::marker::PhantomData;
use std::ptr::NonNull;

use crate::command::{borrowed_cstr_to_string, free_cstr};
use crate::{GENERR, Result, SwitchError, cstring, sys};

/// SQLite result code: `SWITCH_CORE_DB_OK` (0) — the only value FreeSWITCH treats as success for
/// `open`, `exec`, `prepare`, `bind_*`, `finalize`, and `close`.
const DB_OK: i32 = sys::SWITCH_CORE_DB_OK as i32;

/// SQLite result code: `SWITCH_CORE_DB_ROW` (100) — `step` produced another result row.
const DB_ROW: i32 = sys::SWITCH_CORE_DB_ROW as i32;

/// SQLite result code: `SWITCH_CORE_DB_DONE` (101) — `step` finished executing the statement.
const DB_DONE: i32 = sys::SWITCH_CORE_DB_DONE as i32;

/// Maps a SQLite `c_int` result code to `Result<()>`, treating only `SWITCH_CORE_DB_OK` as
/// success. Callers may consult [`CoreDb::errmsg`] for the engine's description of any error.
fn db_result(code: i32) -> Result<()> {
    if code == DB_OK {
        Ok(())
    } else {
        Err(SwitchError(GENERR))
    }
}

/// An owned connection to FreeSWITCH's built-in SQLite database.
///
/// Created with [`CoreDb::open`] (or [`CoreDb::open_uri`] for `file:` URIs). The connection is
/// closed automatically on `Drop` via `switch_core_db_close`, so all [`Stmt`] handles prepared
/// from it must be dropped first — exactly as the Rust borrow checker enforces via [`Stmt`]'s
/// `'a` lifetime.
///
/// ```no_run
/// # use fswtch::CoreDb;
/// let db = CoreDb::open(":memory:")?;
/// db.exec("CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT)")?;
/// let stmt = db.prepare("INSERT INTO t(name) VALUES (?)")?;
/// stmt.bind_text(1, "alice")?;
/// stmt.step()?;
/// assert_eq!(db.changes(), 1);
/// # Ok::<(), fswtch::SwitchError>(())
/// ```
pub struct CoreDb {
    raw: NonNull<sys::switch_core_db_t>,
    // SQLite connections are not thread-safe by default; `exec`/`prepare` mutate through `&self`.
    _marker: PhantomData<*const ()>,
}

impl CoreDb {
    /// Opens (creating if missing) the SQLite database at `filename`, encoded as UTF-8.
    ///
    /// Pass `":memory:"` for a private in-memory database. Returns the new connection on success;
    /// on failure the engine's message is available via the (dropped) error context — reopen and
    /// call [`CoreDb::errmsg`] is not possible since the connection did not survive, so callers
    /// inspecting failure should instead use a fresh handle.
    pub fn open(filename: &str) -> Result<Self> {
        let filename = cstring(filename)?;
        let mut handle = std::ptr::null_mut();
        // SAFETY: `filename` is a valid C string for the duration of the call; `handle` is an
        // out-param that FreeSWITCH populates with the new connection (or NULL on failure).
        let code = unsafe { sys::switch_core_db_open(filename.as_ptr(), &mut handle) };
        db_result(code)?;
        let raw = NonNull::new(handle).ok_or(SwitchError(GENERR))?;
        Ok(Self {
            raw,
            _marker: PhantomData,
        })
    }

    /// Like [`CoreDb::open`] but additionally enables `SQLITE_OPEN_URI` semantics (the `file:`
    /// URI form, with query-string options such as `?mode=memory&cache=shared`).
    pub fn open_uri(filename: &str) -> Result<Self> {
        let filename = cstring(filename)?;
        let mut handle = std::ptr::null_mut();
        // SAFETY: same contract as `switch_core_db_open`, plus URI parsing enabled by the V2 entry.
        let code = unsafe { sys::switch_core_db_open_v2(filename.as_ptr(), &mut handle) };
        db_result(code)?;
        let raw = NonNull::new(handle).ok_or(SwitchError(GENERR))?;
        Ok(Self {
            raw,
            _marker: PhantomData,
        })
    }

    /// Wraps an existing FreeSWITCH database handle.
    ///
    /// # Safety
    ///
    /// `raw` must point to a live `switch_core_db_t` obtained from `switch_core_db_open*` that the
    /// caller intends to transfer ownership of to this wrapper (it will be closed on `Drop`).
    pub unsafe fn from_raw(raw: *mut sys::switch_core_db_t) -> Option<Self> {
        NonNull::new(raw).map(|raw| Self {
            raw,
            _marker: PhantomData,
        })
    }

    #[inline]
    pub fn as_ptr(&self) -> *mut sys::switch_core_db_t {
        self.raw.as_ptr()
    }

    /// Compiles and runs one or more SQL statements with no per-row callback.
    ///
    /// For statements that return rows, use [`CoreDb::prepare`] and iterate the [`Stmt`] instead.
    /// Any error message produced by the engine is freed after this call returns; use
    /// [`CoreDb::errmsg`] afterwards to retrieve the current message.
    pub fn exec(&self, sql: &str) -> Result<()> {
        let sql = cstring(sql)?;
        let mut errmsg: *mut std::os::raw::c_char = std::ptr::null_mut();
        // SAFETY: `self.raw` is a live connection; `sql` is a valid C string; `errmsg` is a valid
        // out-param and NULL callback (third arg) is explicitly permitted by FreeSWITCH.
        let code = unsafe {
            sys::switch_core_db_exec(
                self.raw.as_ptr(),
                sql.as_ptr(),
                None,
                std::ptr::null_mut(),
                &mut errmsg,
            )
        };
        // SAFETY: `free_cstr` is null-safe; when set, `errmsg` is malloc'd by SQLite and owned by
        // us until freed.
        unsafe { free_cstr(errmsg) };
        db_result(code)
    }

    /// Compiles the first statement in `sql` into a byte-code program.
    ///
    /// The returned [`Stmt`] borrows this connection (`'a`) so it cannot outlive the `CoreDb`,
    /// matching SQLite's requirement that statements be finalized before the connection closes.
    pub fn prepare<'a>(&'a self, sql: &str) -> Result<Stmt<'a>> {
        let sql = cstring(sql)?;
        let mut stmt: *mut sys::switch_core_db_stmt_t = std::ptr::null_mut();
        let mut tail: *const std::os::raw::c_char = std::ptr::null();
        // SAFETY: `self.raw` is live; `sql` is a valid C string; `nBytes = -1` reads to the NUL
        // terminator; `stmt` and `tail` are valid out-params.
        let code = unsafe {
            sys::switch_core_db_prepare(
                self.raw.as_ptr(),
                sql.as_ptr(),
                -1,
                &mut stmt,
                &mut tail,
            )
        };
        db_result(code)?;
        let stmt = NonNull::new(stmt).ok_or(SwitchError(GENERR))?;
        Ok(Stmt {
            stmt,
            db: PhantomData,
            bound: Vec::new(),
        })
    }

    /// The engine's description of the most recent `switch_core_db_*` error for this connection.
    ///
    /// Returns `Some("not an error")` when the last call succeeded. The string borrows SQLite
    /// storage tied to the connection and is copied into an owned `String` before returning.
    pub fn errmsg(&self) -> Option<String> {
        // SAFETY: `self.raw` is a live connection; `errmsg` returns a borrowed static/owned string.
        let ptr = unsafe { sys::switch_core_db_errmsg(self.raw.as_ptr()) };
        borrowed_cstr_to_string(ptr)
    }

    /// The number of rows modified by the most recent `INSERT`/`UPDATE`/`DELETE` statement.
    pub fn changes(&self) -> i32 {
        // SAFETY: `self.raw` is a live connection.
        unsafe { sys::switch_core_db_changes(self.raw.as_ptr()) }
    }

    /// The rowid of the most recent successful `INSERT` on this connection.
    pub fn last_insert_rowid(&self) -> i64 {
        // SAFETY: `self.raw` is a live connection.
        unsafe { sys::switch_core_db_last_insert_rowid(self.raw.as_ptr()) }
    }
}

impl Drop for CoreDb {
    fn drop(&mut self) {
        // SAFETY: `self.raw` is the owned connection handle; `switch_core_db_close` releases it.
        // All `Stmt`s borrowing this connection are guaranteed dropped first by the borrow checker.
        unsafe {
            sys::switch_core_db_close(self.raw.as_ptr());
        }
    }
}

/// A compiled SQL statement, borrowed from the [`CoreDb`] that prepared it.
///
/// Call [`Stmt::step`] to advance; when it returns `Ok(false)` the statement is done. Bind
/// parameters with the `bind_*` methods before stepping, and read columns with the `column_*`
/// methods while positioned on a row. The statement is finalized (`switch_core_db_finalize`) on
/// `Drop`.
pub struct Stmt<'a> {
    stmt: NonNull<sys::switch_core_db_stmt_t>,
    db: PhantomData<&'a CoreDb>,
    /// Owned copies of every `&str` bound via `bind_text`, kept alive for the statement's lifetime
    /// so the `SQLITE_STATIC` destructor (null) we hand SQLite remains valid until `finalize`.
    bound: Vec<CString>,
}

impl<'a> Stmt<'a> {
    /// Wraps a prepared-statement handle borrowed from the given connection.
    ///
    /// # Safety
    ///
    /// `stmt` must point to a live `switch_core_db_stmt_t` produced by `switch_core_db_prepare`
    /// on a connection that outlives the returned `Stmt`.
    pub unsafe fn from_raw(stmt: *mut sys::switch_core_db_stmt_t) -> Option<Stmt<'a>> {
        NonNull::new(stmt).map(|stmt| Stmt {
            stmt,
            db: PhantomData,
            bound: Vec::new(),
        })
    }

    #[inline]
    pub fn as_ptr(&self) -> *mut sys::switch_core_db_stmt_t {
        self.stmt.as_ptr()
    }

    /// Advances the statement one step.
    ///
    /// Returns `Ok(true)` when a new row is available (read it with the `column_*` methods) and
    /// `Ok(false)` when the statement has finished executing. Any other SQLite result code is
    /// mapped to an error; consult [`CoreDb::errmsg`] on the owning connection for details.
    pub fn step(&self) -> Result<bool> {
        // SAFETY: `self.stmt` is a live prepared statement.
        let code = unsafe { sys::switch_core_db_step(self.stmt.as_ptr()) };
        match code {
            DB_ROW => Ok(true),
            DB_DONE => Ok(false),
            _ => Err(SwitchError(GENERR)),
        }
    }

    /// Resets the statement to its initial state, ready to be re-executed.
    ///
    /// Bound parameter values are retained per the SQLite contract.
    pub fn reset(&self) -> Result<()> {
        // SAFETY: `self.stmt` is a live prepared statement.
        let code = unsafe { sys::switch_core_db_reset(self.stmt.as_ptr()) };
        db_result(code)
    }

    /// Binds a text value to the parameter at 1-based index `idx`.
    ///
    /// The text is owned by this [`Stmt`] (an owned copy is retained for the statement's lifetime)
    /// and bound with the `SQLITE_STATIC` destructor (null), so SQLite reads the bytes lazily.
    /// SQLite therefore does not copy the text, which avoids the `SQLITE_TRANSIENT` sentinel that
    /// `bindgen` does not emit.
    pub fn bind_text(&mut self, idx: i32, value: &str) -> Result<()> {
        let value = cstring(value)?;
        // SAFETY: `self.stmt` is live; `value` is a valid C string owned by `self.bound` for the
        // statement's lifetime, so the null (SQLITE_STATIC) destructor is sound.
        let code = unsafe {
            sys::switch_core_db_bind_text(
                self.stmt.as_ptr(),
                idx,
                value.as_ptr(),
                -1,
                None,
            )
        };
        self.bound.push(value);
        db_result(code)
    }

    /// Binds a 32-bit integer to the parameter at 1-based index `idx`.
    pub fn bind_int(&self, idx: i32, value: i32) -> Result<()> {
        // SAFETY: `self.stmt` is a live prepared statement.
        let code = unsafe { sys::switch_core_db_bind_int(self.stmt.as_ptr(), idx, value) };
        db_result(code)
    }

    /// Binds a 64-bit integer to the parameter at 1-based index `idx`.
    pub fn bind_int64(&self, idx: i32, value: i64) -> Result<()> {
        // SAFETY: `self.stmt` is a live prepared statement.
        let code = unsafe { sys::switch_core_db_bind_int64(self.stmt.as_ptr(), idx, value) };
        db_result(code)
    }

    /// Binds a double to the parameter at 1-based index `idx`.
    pub fn bind_double(&self, idx: i32, value: f64) -> Result<()> {
        // SAFETY: `self.stmt` is a live prepared statement.
        let code = unsafe { sys::switch_core_db_bind_double(self.stmt.as_ptr(), idx, value) };
        db_result(code)
    }

    /// The number of columns in the result set (0 for statements that produce no rows).
    pub fn column_count(&self) -> i32 {
        // SAFETY: `self.stmt` is a live prepared statement.
        unsafe { sys::switch_core_db_column_count(self.stmt.as_ptr()) }
    }

    /// The UTF-8 text of column `idx` (0-based) on the current row, or `None` for SQL `NULL`.
    ///
    /// The string is copied out of SQLite's owned storage before returning.
    pub fn column_text(&self, idx: i32) -> Option<String> {
        // SAFETY: `self.stmt` is live; the column value borrows SQLite storage tied to the
        // statement, valid until the next `step`/`reset`/`finalize`.
        let ptr = unsafe { sys::switch_core_db_column_text(self.stmt.as_ptr(), idx) };
        if ptr.is_null() {
            return None;
        }
        // SAFETY: SQLite guarantees a NUL-terminated UTF-8 string (or NULL pointer) here.
        let text = unsafe { std::ffi::CStr::from_ptr(ptr.cast()) };
        text.to_str().ok().map(ToOwned::to_owned)
    }

    /// The name (heading) of column `idx` (0-based), or `None` if SQLite returns NULL.
    pub fn column_name(&self, idx: i32) -> Option<String> {
        // SAFETY: `self.stmt` is a live prepared statement; the returned pointer borrows the
        // statement's storage and is copied out below.
        let ptr = unsafe { sys::switch_core_db_column_name(self.stmt.as_ptr(), idx) };
        borrowed_cstr_to_string(ptr)
    }
}

impl Drop for Stmt<'_> {
    fn drop(&mut self) {
        // SAFETY: `self.stmt` is the owned prepared statement; `switch_core_db_finalize` releases
        // it. The borrow on the owning `CoreDb` is still live (the `'a` lifetime).
        unsafe {
            sys::switch_core_db_finalize(self.stmt.as_ptr());
        }
    }
}

/// Iterator over the remaining rows of a [`Stmt`].
///
/// Created by [`Stmt::iter`]; each call to `next` steps the statement and yields `Ok(())` for
/// every row. The iterator ends (yields `None`) when the statement reports `DONE` or on the first
/// error.
pub struct StmtRows<'s, 'db: 's> {
    stmt: &'s Stmt<'db>,
    done: bool,
}

impl<'db> Stmt<'db> {
    /// Returns an iterator that steps the statement once per `next`, yielding `Ok(())` for each
    /// row. Use the `column_*` methods inside the loop body.
    pub fn iter(&self) -> StmtRows<'_, 'db> {
        StmtRows {
            stmt: self,
            done: false,
        }
    }
}

impl<'s, 'db> Iterator for StmtRows<'s, 'db> {
    type Item = Result<()>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.stmt.step() {
            Ok(true) => Some(Ok(())),
            Ok(false) => {
                self.done = true;
                None
            }
            Err(error) => {
                self.done = true;
                Some(Err(error))
            }
        }
    }
}

#[cfg(all(test, feature = "live_fs"))]
mod tests {
    use super::*;

    fn memdb() -> CoreDb {
        CoreDb::open(":memory:").expect("open memory db")
    }

    #[test]
    fn open_and_exec_creates_table() {
        let db = memdb();
        db.exec("CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT)")
            .expect("exec create");
    }

    #[test]
    fn prepare_bind_step_and_read_columns() {
        let db = memdb();
        db.exec("CREATE TABLE t(id INTEGER PRIMARY KEY, s TEXT)")
            .unwrap();
        let mut stmt = db.prepare("INSERT INTO t(s) VALUES (?)").unwrap();
        stmt.bind_text(1, "hello").unwrap();
        assert!(stmt.step().unwrap() == false);
        assert_eq!(db.changes(), 1);

        let q = db.prepare("SELECT id, s FROM t").unwrap();
        assert_eq!(q.column_count(), 2);
        assert!(q.step().unwrap());
        assert_eq!(q.column_text(0).map(|s| s.parse::<i64>().unwrap()), Some(db.last_insert_rowid()));
        assert_eq!(q.column_text(1), Some("hello".to_owned()));
        assert!(!q.step().unwrap());
    }

    #[test]
    fn iterator_yields_one_row() {
        let db = memdb();
        db.exec("CREATE TABLE t(s TEXT)").unwrap();
        db.exec("INSERT INTO t(s) VALUES ('a'), ('b'), ('c')").unwrap();
        let q = db.prepare("SELECT s FROM t ORDER BY s").unwrap();
        let mut values = Vec::new();
        for row in q.iter() {
            row.unwrap();
            values.push(q.column_text(0));
        }
        assert_eq!(values, vec![Some("a".to_owned()), Some("b".to_owned()), Some("c".to_owned())]);
    }

    #[test]
    fn null_text_column_is_none() {
        let db = memdb();
        db.exec("CREATE TABLE t(s TEXT)").unwrap();
        db.exec("INSERT INTO t(s) VALUES (NULL)").unwrap();
        let q = db.prepare("SELECT s FROM t").unwrap();
        q.step().unwrap();
        assert_eq!(q.column_text(0), None);
    }

    #[test]
    fn bad_sql_is_err() {
        let db = memdb();
        assert!(db.prepare("SELECT FROM nowhere").is_err());
    }

    #[test]
    fn errmsg_is_some_after_success() {
        let db = memdb();
        db.exec("CREATE TABLE t(x INTEGER)").unwrap();
        // SQLite returns "not an error" after a successful call.
        assert!(db.errmsg().is_some());
    }
}
