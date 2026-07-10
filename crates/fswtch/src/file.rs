//! FreeSWITCH file I/O — `switch_core_file_*` (audio file handles) + `switch_file_*`
//! (APR file primitives).
//!
//! `switch_file_*` wraps APR file ops; Rust's `std::fs` / `std::io` are the preferred native
//! equivalent for general file I/O — these wrappers exist for parity with the C API surface, so
//! downstream code never has to touch `unsafe` to call them.

use std::ffi::{CStr, c_void};

use crate::{Pool, Result, cstring, status_to_result, sys};

// ── switch_core_file_* (audio file handle: *mut switch_file_handle_t) ─────

/// Reads up to `len` samples into `data` from the open file handle. Returns the actual count in
/// `len`. `data`/`fh` must be valid for the call; `len` is in/out.
pub fn core_file_read(
    fh: *mut sys::switch_file_handle_t,
    data: *mut c_void,
    len: &mut u64,
) -> Result<()> {
    let mut n: sys::switch_size_t = *len as _;
    // SAFETY: `fh` is a live file handle; `data` describes a valid buffer; `&mut n` is a valid out-param.
    let s = unsafe { sys::switch_core_file_read(fh, data, &mut n) };
    *len = n as u64;
    status_to_result(s)
}

/// Writes `len` samples from `data` to the open file handle. Returns the actual count in `len`.
pub fn core_file_write(
    fh: *mut sys::switch_file_handle_t,
    data: *mut c_void,
    len: &mut u64,
) -> Result<()> {
    let mut n: sys::switch_size_t = *len as _;
    // SAFETY: `fh` live; `data` valid; `&mut n` valid out-param.
    let s = unsafe { sys::switch_core_file_write(fh, data, &mut n) };
    *len = n as u64;
    status_to_result(s)
}

/// Writes a video frame to the file handle.
pub fn core_file_write_video(
    fh: *mut sys::switch_file_handle_t,
    frame: *mut sys::switch_frame_t,
) -> Result<()> {
    // SAFETY: `fh` live; `frame` valid for the call.
    status_to_result(unsafe { sys::switch_core_file_write_video(fh, frame) })
}

/// Reads a video frame from the file handle.
pub fn core_file_read_video(
    fh: *mut sys::switch_file_handle_t,
    frame: *mut sys::switch_frame_t,
    flags: sys::switch_video_read_flag_t,
) -> Result<()> {
    // SAFETY: `fh` live; `frame` valid; `flags` is a plain enum value.
    status_to_result(unsafe { sys::switch_core_file_read_video(fh, frame, flags) })
}

/// Seeks within the file handle. `*cur_pos` receives the new position; `samples` is the offset;
/// `whence` is a `SEEK_*`-style int. Returns the new position via `cur_pos`.
pub fn core_file_seek(
    fh: *mut sys::switch_file_handle_t,
    cur_pos: &mut u32,
    samples: i64,
    whence: i32,
) -> Result<()> {
    // SAFETY: `fh` live; `cur_pos` valid out-param; plain integer args.
    status_to_result(unsafe { sys::switch_core_file_seek(fh, cur_pos, samples, whence) })
}

/// Sets a metadata string column (`col`) on the file handle.
pub fn core_file_set_string(
    fh: *mut sys::switch_file_handle_t,
    col: sys::switch_audio_col_t,
    string: impl AsRef<str>,
) -> Result<()> {
    let string = cstring(string)?;
    // SAFETY: `fh` live; `col` valid; `string` valid C string for the call.
    status_to_result(unsafe { sys::switch_core_file_set_string(fh, col, string.as_ptr()) })
}

/// Reads a metadata string column (`col`) from the file handle.
pub fn core_file_get_string(
    fh: *mut sys::switch_file_handle_t,
    col: sys::switch_audio_col_t,
) -> Result<Option<String>> {
    let mut ptr: *const std::os::raw::c_char = std::ptr::null();
    // SAFETY: `fh` live; `col` valid; `&mut ptr` valid out-param.
    let s = unsafe { sys::switch_core_file_get_string(fh, col, &mut ptr) };
    status_to_result(s)?;
    Ok(if ptr.is_null() {
        None
    } else {
        // SAFETY: null or a C string borrowed from the file handle for the call.
        Some(
            unsafe { CStr::from_ptr(ptr) }
                .to_string_lossy()
                .into_owned(),
        )
    })
}

/// Pre-closes a file handle (releases codecs while keeping the handle).
pub fn core_file_pre_close(fh: *mut sys::switch_file_handle_t) -> Result<()> {
    // SAFETY: `fh` live.
    status_to_result(unsafe { sys::switch_core_file_pre_close(fh) })
}

/// Duplicates `oldfh` into `newfh` allocated on `pool`. `newfh` is an out-param.
pub fn core_file_handle_dup(
    oldfh: *mut sys::switch_file_handle_t,
    newfh: *mut *mut sys::switch_file_handle_t,
    pool: &Pool,
) -> Result<()> {
    // SAFETY: `oldfh` live; `newfh` valid out-param; `pool.as_ptr()` is a live APR pool.
    status_to_result(unsafe { sys::switch_core_file_handle_dup(oldfh, newfh, pool.as_ptr()) })
}

/// Closes a file handle (releases codecs + the handle).
pub fn core_file_close(fh: *mut sys::switch_file_handle_t) -> Result<()> {
    // SAFETY: `fh` live.
    status_to_result(unsafe { sys::switch_core_file_close(fh) })
}

/// Sends a control command to the file handle.
pub fn core_file_command(
    fh: *mut sys::switch_file_handle_t,
    command: sys::switch_file_command_t,
) -> Result<()> {
    // SAFETY: `fh` live; `command` valid enum.
    status_to_result(unsafe { sys::switch_core_file_command(fh, command) })
}

/// Truncates the file handle to `offset` bytes.
pub fn core_file_truncate(fh: *mut sys::switch_file_handle_t, offset: i64) -> Result<()> {
    // SAFETY: `fh` live; plain int.
    status_to_result(unsafe { sys::switch_core_file_truncate(fh, offset) })
}

/// `true` if the file handle has a video stream. `check_open` re-checks the open state.
pub fn core_file_has_video(fh: *mut sys::switch_file_handle_t, check_open: bool) -> bool {
    let cb = if check_open {
        sys::switch_bool_t_SWITCH_TRUE
    } else {
        sys::switch_bool_t_SWITCH_FALSE
    };
    // SAFETY: `fh` live.
    unsafe { sys::switch_core_file_has_video(fh, cb) != sys::switch_bool_t_SWITCH_FALSE }
}

// ── switch_file_* (APR file primitives: *mut switch_file_t) ───────────────
// Rust's `std::fs`/`std::io` are the native equivalent; these wrap the APR calls directly.

/// Opens a file. `flag` is an APR open-flags bitmask, `perm` an APR perms value, `pool` the
/// APR pool. Returns a `*mut switch_file_t` handle (null on error → `Err`).
pub fn file_open(
    fname: impl AsRef<str>,
    flag: i32,
    perm: sys::switch_fileperms_t,
    pool: &Pool,
) -> Result<*mut sys::switch_file_t> {
    let fname = cstring(fname)?;
    let mut f: *mut sys::switch_file_t = std::ptr::null_mut();
    // SAFETY: `fname` valid C string; `&mut f` valid out-param; `pool.as_ptr()` live.
    status_to_result(unsafe {
        sys::switch_file_open(&mut f, fname.as_ptr(), flag, perm, pool.as_ptr())
    })?;
    Ok(f)
}

/// Seeks within an APR file. `whence` is a `switch_seek_where_t`; `*offset` is in/out.
pub fn file_seek(
    thefile: *mut sys::switch_file_t,
    whence: sys::switch_seek_where_t,
    offset: &mut i64,
) -> Result<()> {
    // SAFETY: `thefile` live; `offset` valid in/out.
    status_to_result(unsafe { sys::switch_file_seek(thefile, whence, offset) })
}

/// Copies `from_path` to `to_path` with `perms` on `pool`.
pub fn file_copy(
    from_path: impl AsRef<str>,
    to_path: impl AsRef<str>,
    perms: sys::switch_fileperms_t,
    pool: &Pool,
) -> Result<()> {
    let from = cstring(from_path)?;
    let to = cstring(to_path)?;
    // SAFETY: both C strings valid; `pool.as_ptr()` live.
    status_to_result(unsafe {
        sys::switch_file_copy(from.as_ptr(), to.as_ptr(), perms, pool.as_ptr())
    })
}

/// Closes an APR file handle.
pub fn file_close(thefile: *mut sys::switch_file_t) -> Result<()> {
    // SAFETY: `thefile` live.
    status_to_result(unsafe { sys::switch_file_close(thefile) })
}

/// Truncates an APR file to `offset`.
pub fn file_trunc(thefile: *mut sys::switch_file_t, offset: i64) -> Result<()> {
    // SAFETY: `thefile` live; plain int.
    status_to_result(unsafe { sys::switch_file_trunc(thefile, offset) })
}

/// Locks an APR file (`type_` is an APR lock-type int).
pub fn file_lock(thefile: *mut sys::switch_file_t, type_: i32) -> Result<()> {
    // SAFETY: `thefile` live; plain int.
    status_to_result(unsafe { sys::switch_file_lock(thefile, type_) })
}

/// Removes a path. Note: prefer `std::fs::remove_file` for general use.
pub fn file_remove(path: impl AsRef<str>, pool: &Pool) -> Result<()> {
    let path = cstring(path)?;
    // SAFETY: valid C string; live pool.
    status_to_result(unsafe { sys::switch_file_remove(path.as_ptr(), pool.as_ptr()) })
}

/// Renames `from_path` → `to_path`. Prefer `std::fs::rename`.
pub fn file_rename(
    from_path: impl AsRef<str>,
    to_path: impl AsRef<str>,
    pool: &Pool,
) -> Result<()> {
    let from = cstring(from_path)?;
    let to = cstring(to_path)?;
    // SAFETY: both C strings valid; live pool.
    status_to_result(unsafe { sys::switch_file_rename(from.as_ptr(), to.as_ptr(), pool.as_ptr()) })
}

/// Reads up to `*nbytes` into `buf`; returns the actual count in `nbytes`.
pub fn file_read(
    thefile: *mut sys::switch_file_t,
    buf: *mut c_void,
    nbytes: &mut u64,
) -> Result<()> {
    let mut n: sys::switch_size_t = *nbytes as _;
    // SAFETY: `thefile` live; `buf` valid; `&mut n` valid.
    let s = unsafe { sys::switch_file_read(thefile, buf, &mut n) };
    *nbytes = n as u64;
    status_to_result(s)
}

/// Writes `*nbytes` from `buf`; returns the actual count in `nbytes`.
pub fn file_write(
    thefile: *mut sys::switch_file_t,
    buf: *const c_void,
    nbytes: &mut u64,
) -> Result<()> {
    let mut n: sys::switch_size_t = *nbytes as _;
    // SAFETY: `thefile` live; `buf` valid; `&mut n` valid.
    let s = unsafe { sys::switch_file_write(thefile, buf, &mut n) };
    *nbytes = n as u64;
    status_to_result(s)
}

/// Creates a temp file from a `templ` (e.g. `/tmp/fswtch-XXXXXX`; FS overwrites the X's).
/// `templ` must be a writable, NUL-free buffer. Returns the handle via out-param.
///
/// # Safety
/// `templ` must point to a writable buffer that remains valid and large enough for FS to write
/// the generated name back into it.
pub unsafe fn file_mktemp(
    templ: *mut std::os::raw::c_char,
    flags: i32,
    pool: &Pool,
) -> Result<*mut sys::switch_file_t> {
    let mut f: *mut sys::switch_file_t = std::ptr::null_mut();
    // SAFETY: caller guarantees `templ` is writable; live pool.
    status_to_result(unsafe { sys::switch_file_mktemp(&mut f, templ, flags, pool.as_ptr()) })?;
    Ok(f)
}

/// Size of an APR file in bytes.
pub fn file_get_size(thefile: *mut sys::switch_file_t) -> u64 {
    // SAFETY: `thefile` live.
    unsafe { sys::switch_file_get_size(thefile) as u64 }
}

/// `Ok` if the path exists. Prefer `std::path::Path::exists`.
pub fn file_exists(filename: impl AsRef<str>, pool: &Pool) -> Result<()> {
    let filename = cstring(filename)?;
    // SAFETY: valid C string; live pool.
    status_to_result(unsafe { sys::switch_file_exists(filename.as_ptr(), pool.as_ptr()) })
}

/// Creates an APR pipe pair (`in_`/`out` out-params).
pub fn file_pipe_create(
    in_: &mut *mut sys::switch_file_t,
    out: &mut *mut sys::switch_file_t,
    pool: &Pool,
) -> Result<()> {
    // SAFETY: both out-params valid; live pool.
    status_to_result(unsafe { sys::switch_file_pipe_create(in_, out, pool.as_ptr()) })
}

/// Gets a pipe's read timeout.
pub fn file_pipe_timeout_get(
    thepipe: *mut sys::switch_file_t,
    timeout: &mut sys::switch_interval_time_t,
) -> Result<()> {
    // SAFETY: `thepipe` live; `timeout` valid out.
    status_to_result(unsafe { sys::switch_file_pipe_timeout_get(thepipe, timeout) })
}

/// Sets a pipe's read timeout.
pub fn file_pipe_timeout_set(
    thepipe: *mut sys::switch_file_t,
    timeout: sys::switch_interval_time_t,
) -> Result<()> {
    // SAFETY: `thepipe` live; plain value.
    status_to_result(unsafe { sys::switch_file_pipe_timeout_set(thepipe, timeout) })
}
