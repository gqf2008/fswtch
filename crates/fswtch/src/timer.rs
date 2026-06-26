//! Media timer abstraction over FreeSWITCH's `switch_core_timer_*` API.
//!
//! A [`Timer`] owns a `switch_timer_t` (heap-allocated, zeroed before init) backed by a
//! memory pool supplied by the caller. The timer drives media pacing: each call to
//! [`Timer::next`] blocks until the next timer interval elapses, [`Timer::step`] advances
//! the timer without waiting, and [`Timer::check`] polls whether enough samples are ready.
//! The timer is destroyed on drop.

use std::mem::MaybeUninit;

use crate::pool::Pool;
use crate::{Result, cstring, status_to_result, sys};

/// An owned FreeSWITCH media timer.
///
/// Wraps a heap-allocated, zero-initialised `switch_timer_t` (the caller provides the
/// storage, which FreeSWITCH fills in during `switch_core_timer_init`). The timer is
/// destroyed when this value is dropped.
///
/// The borrow on the [`Pool`] used to create it is implicit — the timer's `memory_pool`
/// field aliases the pool, so the `Timer` must not outlive the pool that owns it.
pub struct Timer {
    raw: Box<sys::switch_timer_t>,
}

impl Timer {
    /// Creates and initialises a new timer.
    ///
    /// `name` selects the timer implementation registered with FreeSWITCH (e.g. `"soft"`).
    /// `interval` is the timer interval in milliseconds; `samples` is the number of samples
    /// per interval (typically `sample_rate * interval / 1000`). `pool` supplies the
    /// backing memory pool — the timer aliases it and must not outlive it.
    ///
    /// The underlying `switch_timer_t` is zeroed before `switch_core_timer_init` runs, as the
    /// FreeSWITCH contract requires the storage to be initialised prior to init.
    pub fn new(name: impl AsRef<str>, interval: u32, samples: u32, pool: &Pool) -> Result<Self> {
        let name = cstring(name)?;

        // SAFETY: `MaybeUninit<switch_timer_t>` has no Drop glue, so `new_zeroed_slice` /
        // `Box::<MaybeUninit<_>>::new_zeroed` yields a fully zeroed, aligned allocation.
        // `switch_timer_t` is `#[repr(C)]` with plain POD fields, so all-zero is a valid bit
        // pattern for it.
        let uninit: Box<MaybeUninit<sys::switch_timer_t>> = Box::new(MaybeUninit::zeroed());
        // SAFETY: `uninit` was just zeroed above; all-zero is valid for `switch_timer_t`.
        let mut raw: Box<sys::switch_timer_t> = unsafe { uninit.assume_init() };

        // SAFETY: `raw` points to a zeroed `switch_timer_t` with valid storage for the
        // duration of the call; `name` is a valid C string; `pool.as_ptr()` is a live pool.
        let status = unsafe {
            sys::switch_core_timer_init(
                raw.as_mut(),
                name.as_ptr(),
                interval as ::std::os::raw::c_int,
                samples as ::std::os::raw::c_int,
                pool.as_ptr(),
            )
        };
        status_to_result(status)?;
        Ok(Self { raw })
    }

    #[inline]
    pub fn as_ptr(&self) -> *mut sys::switch_timer_t {
        std::ptr::addr_of!(*self.raw) as *mut sys::switch_timer_t
    }

    /// Blocks until the next timer interval elapses.
    ///
    /// Wraps `switch_core_timer_next`. Returns `Ok(())` on `SWITCH_STATUS_SUCCESS`.
    pub fn tick(&mut self) -> Result<()> {
        // SAFETY: `self.raw` is an initialised, live timer.
        let status = unsafe { sys::switch_core_timer_next(self.raw.as_mut()) };
        status_to_result(status)
    }

    /// Advances the timer by one step without waiting.
    ///
    /// Wraps `switch_core_timer_step`. Use this to move the timer forward without blocking.
    pub fn step(&mut self) -> Result<()> {
        // SAFETY: `self.raw` is an initialised, live timer.
        let status = unsafe { sys::switch_core_timer_step(self.raw.as_mut()) };
        status_to_result(status)
    }

    /// Synchronises the timer to the current time.
    ///
    /// Wraps `switch_core_timer_sync`, which resets the timer's reference point so that
    /// subsequent calls do not attempt to "catch up" to missed intervals.
    pub fn sync(&mut self) -> Result<()> {
        // SAFETY: `self.raw` is an initialised, live timer.
        let status = unsafe { sys::switch_core_timer_sync(self.raw.as_mut()) };
        status_to_result(status)
    }

    /// Polls the timer, optionally stepping it.
    ///
    /// Wraps `switch_core_timer_check`. When `step` is `true` the timer is advanced after the
    /// check; when `false` the timer is inspected without modification. Returns `Ok(())` on
    /// `SWITCH_STATUS_SUCCESS`.
    pub fn check(&mut self, step: bool) -> Result<()> {
        let flag = if step {
            sys::switch_bool_t_SWITCH_TRUE
        } else {
            sys::switch_bool_t_SWITCH_FALSE
        };
        // SAFETY: `self.raw` is an initialised, live timer; `flag` is a valid switch_bool_t.
        let status = unsafe { sys::switch_core_timer_check(self.raw.as_mut(), flag) };
        status_to_result(status)
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        // SAFETY: `self.raw` is an initialised timer that has not yet been destroyed. A null
        // timer interface would indicate the timer was never successfully initialised; the
        // all-zero `switch_timer_t` is the default from `new`, and `switch_core_timer_destroy`
        // tolerates an uninitialised timer (it no-ops when `timer_interface` is NULL), so the
        // guard below only skips the call for the never-inited degenerate case.
        if !self.raw.timer_interface.is_null() {
            unsafe { sys::switch_core_timer_destroy(self.raw.as_mut()) };
        }
        // `raw` (the `Box<switch_timer_t>`) is dropped here, freeing the storage.
    }
}
