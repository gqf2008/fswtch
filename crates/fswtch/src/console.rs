//! FreeSWITCH console: command execution and tab-completion registration.
//!
//! This module wraps the subset of `switch_console.h` that is safe to drive from a native module:
//!
//! - [`execute`] runs an arbitrary FreeSWITCH API command (the same string you would type at the
//!   `fs_cli` console) and returns its textual output as an owned `String`.
//! - [`CompletionFunc`] is an RAII guard that registers a tab-completion callback for the lifetime of
//!   the guard and unregisters it on `Drop`.
//! - [`expand_alias`] expands a console alias, and [`complete`] drives the lower-level completion
//!   machinery against a caller-supplied stream.
//!
//! `switch_console_loop` (the blocking terminal read loop) is intentionally not wrapped — it never
//! returns and is meant to be the console thread's entry point, not something a module calls.

use std::ffi::{CStr, CString, c_char};

use crate::command::borrowed_cstr_to_string;
use crate::{GENERR, Result, SwitchError, cstring, status_to_result, sys};

/// Initial allocation size for a freshly constructed stream, matching FreeSWITCH's
/// `SWITCH_CMD_CHUNK_LEN` / `SWITCH_STANDARD_STREAM` macro.
const STREAM_CHUNK_LEN: usize = 1024;

unsafe extern "C" {
    fn malloc(size: usize) -> *mut std::ffi::c_void;
    fn free(ptr: *mut std::ffi::c_void);
}

/// Allocates a zeroed buffer of `STREAM_CHUNK_LEN` bytes via the libc allocator, matching the
/// expectations of FreeSWITCH's stream writers (which `realloc` the same buffer). Returns `Err` on
/// allocation failure.
fn alloc_chunk() -> Result<*mut u8> {
    // SAFETY: `malloc` is the libc allocator FreeSWITCH's writers pair with via `realloc`; the size
    // is a small compile-time constant.
    let ptr = unsafe { malloc(STREAM_CHUNK_LEN) };
    if ptr.is_null() {
        return Err(SwitchError(GENERR));
    }
    // SAFETY: `ptr` is a fresh allocation of `STREAM_CHUNK_LEN` bytes.
    unsafe { std::ptr::write_bytes(ptr.cast::<u8>(), 0, STREAM_CHUNK_LEN) };
    Ok(ptr.cast())
}

/// Builds a `switch_stream_handle_t` replicating FreeSWITCH's `SWITCH_STANDARD_STREAM` macro: a
/// zeroed struct over a malloc'd `STREAM_CHUNK_LEN` buffer, with both console writers installed.
///
/// Returns the handle and the original buffer pointer. The writers may `realloc` `stream.data`
/// away from the returned buffer, so callers that read output must always use the final
/// `stream.data` / `stream.data_len` rather than this pointer.
fn standard_stream() -> Result<(sys::switch_stream_handle, *mut u8)> {
    let buffer = alloc_chunk()?;
    let stream = sys::switch_stream_handle {
        data: buffer.cast(),
        end: buffer.cast(),
        data_size: STREAM_CHUNK_LEN,
        write_function: Some(sys::switch_console_stream_write),
        raw_write_function: Some(sys::switch_console_stream_raw_write),
        alloc_len: STREAM_CHUNK_LEN,
        alloc_chunk: STREAM_CHUNK_LEN,
        ..Default::default()
    };
    Ok((stream, buffer))
}

/// Runs a FreeSWITCH API command — the same string typed at the `fs_cli` console — and returns its
/// captured output.
///
/// A private `switch_stream_handle_t` is constructed inline (mirroring FreeSWITCH's
/// `SWITCH_STANDARD_STREAM` macro), the command is executed against it with recursion disabled, and
/// the accumulated text is copied out before the stream buffer is freed. The command string must
/// not contain an interior NUL.
///
/// Returns the command's textual output, which may be empty. Failure (unregistered command, stream
/// setup failure, or a non-success status from `switch_console_execute`) is reported via `Err`.
pub fn execute(cmd: impl AsRef<str>) -> Result<String> {
    // `switch_console_execute` takes `char *xcmd` and may tokenize it in place, so hand it a
    // writable, owned, NUL-terminated buffer rather than the `CString`'s const pointer.
    let mut cmd_bytes = cstring(cmd)?.into_bytes_with_nul();
    let cmd_ptr = cmd_bytes.as_mut_ptr().cast::<c_char>();

    let (mut stream, _buffer) = standard_stream()?;

    // SAFETY: `stream` is a fully initialized handle with a valid buffer and the console writers
    // installed; `cmd_ptr` is a writable, NUL-terminated C string valid for the duration of the
    // call. Recursion is disabled (0) so a command cannot re-enter `execute`.
    let status = unsafe { sys::switch_console_execute(cmd_ptr, 0, &mut stream) };

    // Read the accumulated output before tearing the stream down. `data` may have been realloc'd by
    // the writers, so always free the final `data` pointer rather than the original `buffer`.
    let data_ptr = stream.data.cast::<u8>();
    let len = stream.data_len;
    let output = if !data_ptr.is_null() {
        // SAFETY: `data_ptr` is null or points at the null-terminated buffer the writers maintain;
        // `data_len` is the number of bytes written.
        let bytes = unsafe { std::slice::from_raw_parts(data_ptr, len) };
        String::from_utf8_lossy(bytes).into_owned()
    } else {
        String::new()
    };

    // SAFETY: `stream.data` is the current buffer (possibly realloc'd from `buffer`) allocated by
    // the libc allocator and now no longer referenced.
    if !data_ptr.is_null() {
        unsafe { free(data_ptr.cast()) };
    }

    status_to_result(status)?;
    Ok(output)
}

/// Runs a FreeSWITCH API command via `switch_api_execute`, with the command name and argument
/// passed separately and an optional session for command context.
///
/// Unlike [`execute`] (which drives `switch_console_execute` over a single combined command
/// line), this mirrors the `fs_cli` `cmd arg` split: some API commands behave differently when
/// the name and argument are separated, and a subset rely on a live session being attached.
/// Pass `None` for `session` when no session context is needed.
///
/// A private `switch_stream_handle_t` is constructed inline (mirroring `SWITCH_STANDARD_STREAM`),
/// the command is executed against it, and the accumulated text is copied out before the stream
/// buffer is freed. Neither `cmd` nor `arg` may contain an interior NUL.
///
/// Returns the command's textual output, which may be empty. Failure (stream setup failure or a
/// non-success status from `switch_api_execute`) is reported via `Err`.
pub fn execute_api(
    cmd: impl AsRef<str>,
    arg: impl AsRef<str>,
    session: Option<&crate::Session>,
) -> Result<String> {
    let cmd = cstring(cmd)?;
    let arg = cstring(arg)?;
    // A borrowed `Session` is a non-owning handle valid for the call duration; null when absent.
    let session_ptr = session.map_or(std::ptr::null_mut(), |s| s.as_ptr());

    let (mut stream, _buffer) = standard_stream()?;

    // SAFETY: `stream` is a fully initialized handle with a valid buffer and the console writers
    // installed; `cmd`/`arg` are valid, NUL-terminated C strings for the call; `session_ptr` is
    // null or a live session handle.
    let status =
        unsafe { sys::switch_api_execute(cmd.as_ptr(), arg.as_ptr(), session_ptr, &mut stream) };

    // Read the accumulated output before tearing the stream down. `data` may have been realloc'd
    // by the writers, so always free the final `data` pointer rather than the original `buffer`.
    let data_ptr = stream.data.cast::<u8>();
    let len = stream.data_len;
    let output = if !data_ptr.is_null() {
        // SAFETY: `data_ptr` is null or points at the buffer the writers maintain; `data_len` is
        // the number of bytes written.
        let bytes = unsafe { std::slice::from_raw_parts(data_ptr, len) };
        String::from_utf8_lossy(bytes).into_owned()
    } else {
        String::new()
    };

    // SAFETY: `stream.data` is the current buffer (possibly realloc'd from `buffer`) allocated by
    // the libc allocator and now no longer referenced.
    if !data_ptr.is_null() {
        unsafe { free(data_ptr.cast()) };
    }

    status_to_result(status)?;
    Ok(output)
}

/// Expands a console alias, returning the fully-resolved command text.
///
/// `cmd` is the alias name and `arg` is an optional argument to append. Both must be valid C
/// strings (no interior NUL); pass an empty `arg` when there is none.
///
/// Returns `Ok(None)` when no expansion is produced. The returned string is treated as borrowed
/// FreeSWITCH storage and copied out without freeing — the header does not document the returned
/// pointer as malloc'd, so this avoids a potential double-free. Callers for whom a small leak on
/// alias expansion is unacceptable should drive the raw `switch_console_expand_alias` symbol
/// directly once FreeSWITCH's ownership contract is confirmed.
pub fn expand_alias(cmd: impl AsRef<str>, arg: &str) -> Result<Option<String>> {
    // Both pointers are `char *`; treat them as potentially-mutable by handing over writable,
    // NUL-terminated owned buffers.
    let mut cmd_bytes = cstring(cmd)?.into_bytes_with_nul();
    let mut arg_bytes = cstring(arg)?.into_bytes_with_nul();
    let cmd_ptr = cmd_bytes.as_mut_ptr().cast::<c_char>();
    let arg_ptr = arg_bytes.as_mut_ptr().cast::<c_char>();
    // SAFETY: `cmd_ptr` and `arg_ptr` are writable, NUL-terminated C strings valid for the call.
    let expanded = unsafe { sys::switch_console_expand_alias(cmd_ptr, arg_ptr) };
    Ok(borrowed_cstr_to_string(expanded.cast_const()))
}

/// Drives FreeSWITCH's tab-completion machinery against `line` (the full input line) and `last_word`
/// (the token being completed), writing candidate matches into `stream`.
///
/// This is the low-level completion entry point; most modules want [`CompletionFunc`] instead. The
/// `stream` is borrowed for the duration of the call. Returns `true` when FreeSWITCH reports that
/// completion candidates were produced (nonzero status byte), `false` otherwise.
pub fn complete(line: &str, last_word: &str, stream: &mut crate::Stream) -> Result<bool> {
    let line = cstring(line)?;
    let last_word = cstring(last_word)?;
    // SAFETY: `line` and `last_word` are valid C strings; `stream.as_ptr()` is a live handle. A null
    // console output and xml pointer are permitted by the ABI.
    let result = unsafe {
        sys::switch_console_complete(
            line.as_ptr(),
            last_word.as_ptr(),
            std::ptr::null_mut(),
            stream.as_ptr(),
            std::ptr::null_mut(),
        )
    };
    Ok(result != 0)
}

/// An RAII guard for a registered console tab-completion callback.
///
/// Created with [`CompletionFunc::new`], which calls `switch_console_add_complete_func`. When the
/// guard is dropped it calls `switch_console_del_complete_func` with the same name, so the callback
/// must outlive every guard that references it (typically it is a `static` C trampoline).
///
/// The `callback` receives `(func, line, &mut matches)`: the completion function name, the input
/// line so far, and a match list to populate via the raw `switch_console_push_match` family.
pub struct CompletionFunc {
    name: CString,
}

impl CompletionFunc {
    /// Registers `callback` under `name` for the lifetime of the returned guard.
    ///
    /// `name` must be a valid C string (no interior NUL) and should be unique among registered
    /// completion functions. `callback` is stored by FreeSWITCH by reference, so it must remain
    /// valid until the guard is dropped — use a `static` `unsafe extern "C" fn`.
    pub fn new(
        name: impl AsRef<str>,
        callback: sys::switch_console_complete_callback_t,
    ) -> Result<Self> {
        let name = cstring(name)?;
        // SAFETY: `name` is a valid C string; `callback` is a function pointer whose lifetime the
        // caller guarantees.
        let status = unsafe { sys::switch_console_add_complete_func(name.as_ptr(), callback) };
        status_to_result(status)?;
        Ok(Self { name })
    }

    /// The registered completion-function name, for diagnostics.
    pub fn name(&self) -> &CStr {
        &self.name
    }
}

impl Drop for CompletionFunc {
    fn drop(&mut self) {
        // SAFETY: `self.name` was successfully registered by `new` and is still registered (the
        // guard owns the single registration).
        let status = unsafe { sys::switch_console_del_complete_func(self.name.as_ptr()) };
        if status != crate::SUCCESS.raw() {
            // De-registration best-effort in `Drop`; surface failure via the error log rather than
            // panicking.
            crate::log_error(
                "console",
                "switch_console_del_complete_func failed during Drop",
            );
        }
    }
}

/// Frees a match list produced by the completion machinery.
///
/// This is a thin wrapper over `switch_console_free_matches` for callers that drive
/// [`complete`] or `switch_console_run_complete_func` directly and need to release the resulting
/// match list. Pass the list by mutable reference; it is left null on return.
///
/// # Safety
///
/// `matches` must point to writable storage holding either null or a list allocated by FreeSWITCH's
/// completion functions (`switch_console_complete`, `switch_console_run_complete_func`, or the
/// `switch_console_push_match` family).
pub unsafe fn free_matches(matches: &mut *mut sys::switch_console_callback_match_t) {
    // SAFETY: The caller guarantees `matches` points to storage holding null or a FreeSWITCH-allocated
    // match list.
    unsafe { sys::switch_console_free_matches(matches) };
}

/// Iterator over the candidate strings in a [`switch_console_callback_match_t`] match list.
///
/// Yields each `val` in the singly-linked list without taking ownership of the list (the list is
/// freed separately, e.g. via [`free_matches`]).
pub struct CompletionMatches<'a> {
    current: Option<&'a sys::switch_console_callback_match_node>,
}

impl<'a> CompletionMatches<'a> {
    /// Wraps a borrowed match list for iteration.
    ///
    /// The list is borrowed for the lifetime `'a`, so the returned iterator cannot outlive it. The
    /// caller is responsible for ensuring `matches` outlives iteration and is freed afterwards via
    /// [`free_matches`]. Returns `None` when the list has no candidate nodes.
    ///
    /// Callers holding a raw `*mut switch_console_callback_match_t` should first obtain a borrow
    /// via `matches.as_ref()` (after a null check) so the lifetime is anchored to a local and
    /// cannot be extended to `'static`.
    pub fn from_list(matches: &'a sys::switch_console_callback_match_t) -> Option<Self> {
        let head = matches.head;
        if head.is_null() {
            return None;
        }
        // SAFETY: `head` is the first node of the borrowed list, valid for the list's lifetime `'a`.
        Some(Self {
            current: Some(unsafe { &*head }),
        })
    }
}

impl<'a> Iterator for CompletionMatches<'a> {
    type Item = &'a CStr;

    fn next(&mut self) -> Option<Self::Item> {
        let node = self.current.take()?;
        // SAFETY: `node.val`, when non-null, is a valid C string owned by the match list.
        let val = unsafe { CStr::from_ptr(node.val) };
        if node.next.is_null() {
            self.current = None;
        } else {
            // SAFETY: `node.next` is the next node in the same live list.
            self.current = Some(unsafe { &*node.next });
        }
        Some(val)
    }
}
