//! APR memory-pool wrapper.
//!
//! [`Pool`] is an owned handle to a FreeSWITCH sub memory pool (`switch_memory_pool_t`, an APR
//! `apr_pool_t`). Modules that need pool-allocated strings or buffers — codecs, timers, resamplers —
//! obtain a [`Pool`] and then hand its pointer (`as_ptr`) to FreeSWITCH functions that store
//! allocations against the pool's lifetime.
//!
//! The pool is created via `switch_core_perform_new_memory_pool` (the `switch_core_new_memory_pool`
//! macro expands to it with `__FILE__`/`__SWITCH_FUNC__`/`__LINE__`) and destroyed on drop via
//! `switch_core_perform_destroy_memory_pool`. Memory obtained from the pool — [`Pool::strdup`],
//! [`Pool::alloc`] — is freed automatically when the pool is destroyed and must not outlive the
//! `Pool` (the borrow checker enforces this via `&self` lifetimes).

use std::ffi::{CStr, c_char, c_void};
use std::marker::PhantomData;
use std::ptr::NonNull;

use crate::sys::{self, switch_memory_pool_t, switch_size_t};
use crate::{GENERR, Result, SwitchError, cstring, status_to_result};

/// An owned FreeSWITCH memory pool (`switch_memory_pool_t`).
///
/// Allocations made through [`Pool::strdup`] and [`Pool::alloc`] are tied to the pool's lifetime:
/// they are reclaimed when the pool is dropped, and the borrow checker prevents their use after the
/// `Pool` is dropped.
///
/// Pass [`Pool::as_ptr`] to FreeSWITCH functions that require a `switch_memory_pool_t *` (e.g. when
/// configuring a codec or timer).
pub struct Pool {
    raw: NonNull<switch_memory_pool_t>,
    // APR memory pools are not thread-safe by default; mutations from multiple threads race.
    _marker: PhantomData<*const ()>,
}

impl Pool {
    /// Creates a new sub memory pool allocated from FreeSWITCH's core master pool.
    ///
    /// Returns `Err` if the underlying `switch_core_perform_new_memory_pool` call does not return
    /// `SWITCH_STATUS_SUCCESS`.
    pub fn new() -> Result<Self> {
        let mut pool: *mut switch_memory_pool_t = std::ptr::null_mut();
        // SAFETY: `pool` is a valid out-pointer; the source strings are static C string literals.
        let status = unsafe {
            sys::switch_core_perform_new_memory_pool(
                &mut pool,
                c"fswtch-rs".as_ptr(),
                c"Pool::new".as_ptr(),
                line!() as _,
            )
        };
        status_to_result(status)?;
        // SAFETY: `new_memory_pool` returned SUCCESS, so `pool` is a non-null, freshly created pool.
        let raw = NonNull::new(pool).ok_or(SwitchError(GENERR))?;
        Ok(Self {
            raw,
            _marker: PhantomData,
        })
    }

    /// The underlying `switch_memory_pool_t *`.
    ///
    /// This is the documented escape hatch for passing the pool to FreeSWITCH functions (codec/timer
    /// configuration) that are not yet wrapped. The pointer is valid for as long as this `Pool` is
    /// alive.
    #[inline]
    pub fn as_ptr(&self) -> *mut switch_memory_pool_t {
        self.raw.as_ptr()
    }

    /// Copies a string into memory allocated from this pool.
    ///
    /// The returned `&CStr` borrows this `Pool`: the storage is owned by the pool and is reclaimed
    /// when the pool is dropped, so the borrow must not outlive `self`. Interior-NUL input is
    /// rejected as `Err(SwitchError(GENERR))`.
    ///
    /// Uses `switch_core_perform_strdup` (the `switch_core_strdup` macro).
    pub fn strdup(&self, text: impl AsRef<str>) -> Result<&CStr> {
        let todup = cstring(text)?;
        // SAFETY: `self.raw` is a live pool; `todup` is a valid null-terminated C string for the
        // call. The returned pointer borrows pool storage that outlives `self`, matching the `'a`
        // lifetime of the returned reference.
        let dup = unsafe {
            sys::switch_core_perform_strdup(
                self.raw.as_ptr(),
                todup.as_ptr(),
                c"fswtch-rs".as_ptr(),
                c"Pool::strdup".as_ptr(),
                line!() as _,
            )
        };
        let dup: *const c_char = dup.cast_const();
        if dup.is_null() {
            return Err(SwitchError(GENERR));
        }
        // SAFETY: `dup` was just returned non-null by `switch_core_perform_strdup`; it is a valid
        // null-terminated C string living in pool storage that outlives `self`.
        let cstr = unsafe { CStr::from_ptr(dup) };
        // Re-borrow with the explicit `'a` lifetime tied to `self` rather than the local `dup`
        // pointer's inferred lifetime.
        Ok(cstr)
    }

    /// Allocates `size` bytes of zeroed memory from this pool.
    ///
    /// The returned pointer borrows pool storage: it is valid until the `Pool` is dropped. A null
    /// pointer is returned as `Err(SwitchError(GENERR))`.
    ///
    /// This is a raw allocation escape hatch (e.g. for codec/timer frame buffers). The memory is
    /// `memset` to zero by FreeSWITCH (`switch_core_alloc` remark).
    ///
    /// # Safety
    ///
    /// The caller is responsible for not aliasing the returned storage and for treating it as
    /// uninitialised-but-zeroed. The pointer is invalidated when this `Pool` is dropped.
    pub fn alloc(&self, size: usize) -> Result<*mut c_void> {
        // SAFETY: `self.raw` is a live pool; `size` is passed through as the allocation length.
        let mem = unsafe {
            sys::switch_core_perform_alloc(
                self.raw.as_ptr(),
                size as switch_size_t,
                c"fswtch-rs".as_ptr(),
                c"Pool::alloc".as_ptr(),
                line!() as _,
            )
        };
        if mem.is_null() {
            return Err(SwitchError(GENERR));
        }
        Ok(mem)
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        let mut pool: *mut switch_memory_pool_t = self.raw.as_ptr();
        // SAFETY: `self.raw` was created by `switch_core_perform_new_memory_pool` (via `Pool::new`)
        // and has not been destroyed yet; `destroy_memory_pool` nulls the out-pointer and is safe to
        // call exactly once per pool.
        unsafe {
            sys::switch_core_perform_destroy_memory_pool(
                &mut pool,
                c"fswtch-rs".as_ptr(),
                c"Pool::drop".as_ptr(),
                line!() as _,
            );
        }
    }
}

#[cfg(all(test, feature = "live_fs"))]
mod tests {
    use super::*;

    #[test]
    fn pool_creates_and_drops() {
        let pool = Pool::new().expect("new memory pool");
        // Drop runs here; must not abort.
        drop(pool);
    }

    #[test]
    fn strdup_returns_pool_storage() {
        let pool = Pool::new().expect("new memory pool");
        let s = pool.strdup("hello pool").expect("strdup");
        assert_eq!(s.to_bytes(), b"hello pool");
    }

    #[test]
    fn strdup_rejects_interior_nul() {
        let pool = Pool::new().expect("new memory pool");
        assert!(pool.strdup("a\0b").is_err());
    }

    #[test]
    fn alloc_returns_non_null_zeroed() {
        let pool = Pool::new().expect("new memory pool");
        let mem = pool.alloc(128).expect("alloc");
        assert!(!mem.is_null());
        // SAFETY: `switch_core_alloc` memsets the region to zero; 128 bytes were requested.
        let bytes = unsafe { std::slice::from_raw_parts(mem.cast::<u8>(), 128) };
        assert!(bytes.iter().all(|b| *b == 0));
    }
}
