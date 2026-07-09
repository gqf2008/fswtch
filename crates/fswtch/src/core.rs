//! FreeSWITCH core helpers — global variables, identifiers, and runtime introspection.
//!
//! These wrap the `switch_core_get_*` / `switch_core_set_*` family from `switch_core.h` as free
//! functions. They operate on process-global core state and need no handle, pool, or session.

use crate::command::{borrowed_cstr_to_string, strdup_to_string};
use crate::{Result, cstring, sys};

/// Retrieves a global core variable into an owned [`String`].
///
/// Uses `switch_core_get_variable_dup`, which returns a freshly `malloc`'d copy of the value
/// (independent of the core's internal hash storage). The copy is freed after reading, so the
/// returned [`String`] is not invalidated by later [`set_variable`] calls and does not borrow the
/// core. Returns `Ok(None)` when the variable is unset.
///
/// Interior NUL in `name` is rejected as [`crate::SwitchError`](`crate::GENERR`).
pub fn get_variable(name: impl AsRef<str>) -> Result<Option<String>> {
    let name = cstring(name)?;
    // SAFETY: `name` is a valid C string for the duration of the call. The returned pointer is
    // either null (unset) or a malloc'd null-terminated string that `strdup_to_string` copies out
    // and frees.
    let value = unsafe { sys::switch_core_get_variable_dup(name.as_ptr()) };
    // SAFETY: `value` is null or a malloc'd C string per the call contract above.
    Ok(unsafe { strdup_to_string(value) })
}

/// Sets a global core variable. Pass `None` for `value` to delete the variable.
///
/// Interior NUL in `name` or `value` is rejected as [`crate::SwitchError`](`crate::GENERR`).
pub fn set_variable(name: impl AsRef<str>, value: Option<&str>) -> Result<()> {
    let name = cstring(name)?;
    // An owned CString keeps the C string alive for the call even when the caller passes a
    // temporary slice; interior NUL is rejected here so it cannot be silently truncated.
    let value = match value {
        Some(text) => Some(cstring(text)?),
        None => None,
    };
    let value_ptr = value.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
    // SAFETY: `name` and (when present) `value` are valid C strings for the call. A null value
    // pointer is explicitly permitted by the contract to delete the variable.
    unsafe { sys::switch_core_set_variable(name.as_ptr(), value_ptr) };
    Ok(())
}

/// Retrieves the core's unique identifier (a UUID string) into an owned [`String`].
///
/// `switch_core_get_uuid` returns a pointer to static storage holding a UUID; the value is copied
/// here. Returns `None` only if the core returns a null pointer (it never does in practice).
pub fn get_uuid() -> Option<String> {
    // SAFETY: The function takes no arguments and returns a static null-terminated string.
    let ptr = unsafe { sys::switch_core_get_uuid() };
    borrowed_cstr_to_string(ptr.cast_const())
}

/// Retrieves the configured default domain into an owned [`String`].
///
/// Wraps `switch_core_get_domain(dup)`. When `dup` is `true` the core returns a freshly `malloc`'d
/// copy (freed after reading); when `false` it returns a borrowed pointer into static storage that
/// is copied before return. Either way the caller receives an owned [`String`].
///
/// Returns `Ok(None)` when the domain is unset / the core returns null.
pub fn get_domain(dup: bool) -> Result<Option<String>> {
    let flag = if dup {
        sys::switch_bool_t_SWITCH_TRUE
    } else {
        sys::switch_bool_t_SWITCH_FALSE
    };
    // SAFETY: `flag` is a valid `switch_bool_t` enumerator. The returned pointer is null or a
    // null-terminated string; when `dup` is true it is malloc'd, when false it is static.
    let ptr = unsafe { sys::switch_core_get_domain(flag) };
    if ptr.is_null() {
        return Ok(None);
    }
    if dup {
        // SAFETY: With `dup == SWITCH_TRUE` the pointer is malloc'd; `strdup_to_string` copies it
        // out and frees it.
        Ok(unsafe { strdup_to_string(ptr) })
    } else {
        // Borrowed static storage; copy out without freeing.
        Ok(borrowed_cstr_to_string(ptr.cast_const()))
    }
}

/// The FreeSWITCH host name as configured at core startup.
///
/// `switch_core_get_hostname` returns a borrowed `*const c_char` pointing into static storage; the
/// value is copied here. Returns `None` if the core returns null.
pub fn get_hostname() -> Option<String> {
    // SAFETY: No arguments; returns null or a static null-terminated string.
    let ptr = unsafe { sys::switch_core_get_hostname() };
    borrowed_cstr_to_string(ptr)
}

/// The FreeSWITCH switch name (often the same as the hostname).
///
/// `switch_core_get_switchname` returns a borrowed `*const c_char` pointing into static storage;
/// the value is copied here. Returns `None` if the core returns null.
pub fn get_switchname() -> Option<String> {
    // SAFETY: No arguments; returns null or a static null-terminated string.
    let ptr = unsafe { sys::switch_core_get_switchname() };
    borrowed_cstr_to_string(ptr)
}

/// The number of currently active channels in the core.
///
/// Wraps `switch_core_session_count`. Thread-safe — it reads an atomic core counter.
pub fn session_count() -> u32 {
    // SAFETY: No arguments; reads a process-global counter.
    unsafe { sys::switch_core_session_count() }
}

/// FreeSWITCH process uptime, following the upstream `switch_time_t` semantics of
/// `switch_core_uptime` (seconds on current FreeSWITCH releases).
pub fn uptime() -> i64 {
    // SAFETY: No arguments; returns a `switch_time_t` (a 64-bit integer).
    unsafe { sys::switch_core_uptime() }
}

/// Reads or sets the sessions-per-second limit. Pass `0` to read the current value without
/// modifying it; pass a nonzero limit to apply it, returning the previous value.
///
/// Wraps `switch_core_sessions_per_second`. Thread-safe.
pub fn sessions_per_second(limit: u32) -> u32 {
    // SAFETY: `limit` is a plain `u32`; the function reads/updates a core counter.
    unsafe { sys::switch_core_sessions_per_second(limit) }
}

// NOTE: `switch_core_sprintf(pool, fmt, ...)` is intentionally NOT wrapped. It is variadic and
// requires a `switch_memory_pool_t*` (a FreeSWITCH/APR memory pool), which is not part of this
// module's surface. Once a `Pool` wrapper exists, a safe `sprintf(pool, fmt, args)` helper can be
// added alongside it.
