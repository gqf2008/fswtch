//! Safe wrapper over FreeSWITCH's limit subsystem (call caps and rate/interval limiting).
//!
//! FreeSWITCH's limit API is interface-based: a backend module (e.g. `hash`, `db`, `redis`,
//! `array`) registers a [`sys::switch_limit_interface`] and the core exposes thin dispatch
//! functions that take the backend's name as a string. This wrapper covers those dispatch
//! functions; the backend must already be loaded as a FreeSWITCH module for the calls to
//! succeed.
//!
//! Resources are addressed by the `(realm, resource)` pair. `incr` raises a counter (and
//! optionally a per-interval rate); `release` lowers it; `usage` reads it; `reset` and
//! `interval_reset` clear counters. `status` returns backend-specific diagnostic text.
//!
//! `incr` returns [`crate::Result`] where `Ok` means the resource is under the requested
//! limit (the counter was incremented) and `Err` means the limit was exceeded — map onto
//! your call-control decision accordingly.

use crate::{Result, cstring, status_to_result, sys};

/// The well-known limit backends shipped with FreeSWITCH.
///
/// These are convenience constants for the `backend` string argument; any backend that has
/// registered itself as a loadable module may also be addressed by name.
pub mod backend {
    /// The in-memory `hash` backend (no external store).
    pub const HASH: &str = "hash";
    /// The SQL `db` backend.
    pub const DB: &str = "db";
    /// The `redis` backend.
    pub const REDIS: &str = "redis";
    /// The `array` backend (per-call, in-process).
    pub const ARRAY: &str = "array";
}

/// The result of [`usage`]: the current counter value and, when a backend reports it, the
/// rate counter for the active interval.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    /// The current usage count for the `(realm, resource)` pair.
    pub count: i32,
    /// The rate counter for the current interval, when the backend tracks one.
    pub rate: u32,
}

/// Increment the usage counter for `(realm, resource)` on `backend`.
///
/// `max` caps the counter: `0` means "no limit, just count". `interval` is the rate window
/// in seconds: `0` means "no interval" (a plain cap). When the resource is already at `max`
/// the counter is **not** incremented and this returns `Err` — treat that as the limit being
/// exceeded. On success the counter was incremented.
///
/// `session`, when provided, ties the counter to the lifetime of the call: the backend will
/// auto-release the slot when the session hangs up. Pass `None` for a manual (unbound)
/// counter that you must [`release`] yourself.
pub fn incr(
    backend: &str,
    realm: &str,
    resource: &str,
    max: i32,
    interval: i32,
    session: Option<&crate::Session>,
) -> Result<()> {
    let backend = cstring(backend)?;
    let realm = cstring(realm)?;
    let resource = cstring(resource)?;
    let session_ptr = match session {
        Some(s) => s.as_ptr(),
        None => std::ptr::null_mut(),
    };
    // SAFETY: all three C strings are valid NUL-terminated values; `session_ptr` is either a
    // live session pointer obtained from `Session::as_ptr` or null (which the backend treats
    // as "no session").
    let status = unsafe {
        sys::switch_limit_incr(
            backend.as_ptr(),
            session_ptr,
            realm.as_ptr(),
            resource.as_ptr(),
            max,
            interval,
        )
    };
    status_to_result(status)
}

/// Release one unit of usage for `(realm, resource)` on `backend`.
///
/// `session`, when provided, must match the session the slot was acquired with. Returns
/// `Err` if the backend cannot release (e.g. the counter is already zero).
pub fn release(
    backend: &str,
    realm: &str,
    resource: &str,
    session: Option<&crate::Session>,
) -> Result<()> {
    let backend = cstring(backend)?;
    let realm = cstring(realm)?;
    let resource = cstring(resource)?;
    let session_ptr = match session {
        Some(s) => s.as_ptr(),
        None => std::ptr::null_mut(),
    };
    // SAFETY: all three C strings are valid NUL-terminated values; `session_ptr` is either a
    // live session pointer or null.
    let status = unsafe {
        sys::switch_limit_release(
            backend.as_ptr(),
            session_ptr,
            realm.as_ptr(),
            resource.as_ptr(),
        )
    };
    status_to_result(status)
}

/// Reads the current usage for `(realm, resource)` on `backend`.
///
/// Returns the count plus the rate counter for the active interval (the rate is `0` when the
/// backend does not track one). `count` is `-1` when the backend cannot report usage.
pub fn usage(backend: &str, realm: &str, resource: &str) -> Usage {
    let backend = match cstring(backend) {
        Ok(c) => c,
        Err(_) => return Usage::default(),
    };
    let realm = match cstring(realm) {
        Ok(c) => c,
        Err(_) => return Usage::default(),
    };
    let resource = match cstring(resource) {
        Ok(c) => c,
        Err(_) => return Usage::default(),
    };
    let mut rate: u32 = 0;
    // SAFETY: all three C strings are valid NUL-terminated values; `rate` is a valid u32
    // out-pointer.
    let count = unsafe {
        sys::switch_limit_usage(
            backend.as_ptr(),
            realm.as_ptr(),
            resource.as_ptr(),
            &mut rate,
        )
    };
    Usage { count, rate }
}

/// Resets the interval rate counter for `(realm, resource)` on `backend` (the absolute cap
/// counter is left untouched).
pub fn interval_reset(backend: &str, realm: &str, resource: &str) -> Result<()> {
    let backend = cstring(backend)?;
    let realm = cstring(realm)?;
    let resource = cstring(resource)?;
    // SAFETY: all three C strings are valid NUL-terminated values.
    let status =
        unsafe { sys::switch_limit_interval_reset(backend.as_ptr(), realm.as_ptr(), resource.as_ptr()) };
    status_to_result(status)
}

/// Resets every usage counter on `backend`.
pub fn reset(backend: &str) -> Result<()> {
    let backend = cstring(backend)?;
    // SAFETY: `backend` is a valid NUL-terminated C string.
    let status = unsafe { sys::switch_limit_reset(backend.as_ptr()) };
    status_to_result(status)
}

/// Fires a `limit::usage` event carrying the supplied counters for `(realm, resource)` on
/// `backend`.
///
/// This does not change any counter; it only notifies listeners (e.g. a billing or
/// monitoring system) of the current values.
pub fn fire_event(
    backend: &str,
    realm: &str,
    resource: &str,
    usage: u32,
    rate: u32,
    max: u32,
    ratemax: u32,
) -> Result<()> {
    let backend = cstring(backend)?;
    let realm = cstring(realm)?;
    let resource = cstring(resource)?;
    // SAFETY: all three C strings are valid NUL-terminated values; the numeric arguments are
    // passed by value.
    unsafe {
        sys::switch_limit_fire_event(
            backend.as_ptr(),
            realm.as_ptr(),
            resource.as_ptr(),
            usage,
            rate,
            max,
            ratemax,
        )
    };
    Ok(())
}

/// Retrieves backend-specific status text.
///
/// The returned string is malloc-allocated by FreeSWITCH and freed here after copying, so
/// the returned `String` is owned and self-contained. Returns `None` when the backend has no
/// status to report.
pub fn status(backend: &str) -> Result<Option<String>> {
    let backend = cstring(backend)?;
    // SAFETY: `backend` is a valid NUL-terminated C string. The returned pointer is null or a
    // malloc'd null-terminated string owned by the caller (per the header: "caller must free
    // returned value"); `strdup_to_string` copies it out and frees it.
    let ptr = unsafe { sys::switch_limit_status(backend.as_ptr()) };
    // SAFETY: `ptr` is null or a malloc'd C string as above.
    Ok(unsafe { crate::strdup_to_string(ptr) })
}

/// Initialize the limit core. Generally called once by the FreeSWITCH core during startup.
///
/// Exposed for completeness and testing harnesses that bring up the limit subsystem outside
/// the normal core path. The `pool` is borrowed for the lifetime of the call only.
///
/// # Safety
///
/// `pool` must point to a live `switch_memory_pool_t` that remains valid for the duration of
/// the call. Callers normally never need this — the core invokes it during startup.
pub unsafe fn init(pool: *mut sys::switch_memory_pool_t) {
    // SAFETY: upheld by the caller via the `# Safety` contract above.
    unsafe { sys::switch_limit_init(pool) };
}

