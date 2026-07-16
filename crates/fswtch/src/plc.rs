//! Packet Loss Concealment (PLC) — an owned wrapper over FreeSWITCH's `switch_plc_state_t`.
//!
//! PLC masks audible dropouts in a received audio stream by synthesizing replacement
//! samples for lost packets. Received audio is fed in through [`Plc::rx`], which also folds
//! concealment back into the buffer in place; when a packet is missing, [`Plc::fillin`]
//! generates `len` samples of synthesized audio derived from the recent reception history.
//!
//! The handle is owned: [`Plc::new`] allocates the underlying `switch_plc_state_t` (spandsp
//! heap-allocates it inside `switch_plc_init`) and [`Drop`] calls `switch_plc_free`, so a
//! `Plc` cleans up after itself like any other RAII guard.

use std::marker::PhantomData;
use std::ptr::NonNull;

use crate::{GENERR, Result, SwitchError, log_error, sys};

/// An owned packet-loss-concealment generator.
///
/// Allocated with [`Plc::new`] and destroyed on [`Drop`] via `switch_plc_free`. Feed received
/// PCM in with [`rx`](Self::rx) (which may modify the buffer in place) and synthesize
/// replacement audio with [`fillin`](Self::fillin).
///
/// `switch_plc_state_t` is an opaque, non-thread-safe handle, so `Plc` is neither [`Send`] nor
/// [`Sync`] — the `rx`/`fillin` methods mutate C state through `&self`.
pub struct Plc {
    raw: NonNull<sys::switch_plc_state_t>,
    // `switch_plc_state_t` is not thread-safe; `rx`/`fillin` mutate C state through `&self`.
    _marker: PhantomData<*const ()>,
}

impl Plc {
    /// Creates a new PLC instance.
    ///
    /// Spandsp heap-allocates the `plc_state_t` inside `switch_plc_init` when passed a null
    /// handle, so the returned pointer is owned by this wrapper and freed on [`Drop`]. Returns
    /// [`crate::SwitchError`](`crate::GENERR`) if allocation fails (init returns null).
    pub fn new() -> Result<Self> {
        // SAFETY: Passing a null pointer asks spandsp to allocate a fresh `plc_state_t` on the
        // heap. The call has no preconditions beyond a valid out-pointer; a null return signals
        // allocation failure.
        let raw = unsafe { sys::switch_plc_init(std::ptr::null_mut()) };
        NonNull::new(raw)
            .map(|raw| Self {
                raw,
                _marker: PhantomData,
            })
            .ok_or(SwitchError(GENERR))
    }

    /// Wraps an existing `switch_plc_state_t` created elsewhere.
    ///
    /// # Safety
    ///
    /// `raw` must point to a live `switch_plc_state_t` that the caller is willing to hand over
    /// for destruction via `switch_plc_free` when this `Plc` is dropped. Wrapping a handle
    /// already owned by another `Plc` (or any other RAII guard) is unsound — it would be freed
    /// twice.
    pub(crate) unsafe fn from_raw(raw: *mut sys::switch_plc_state_t) -> Option<Self> {
        NonNull::new(raw).map(|raw| Self {
            raw,
            _marker: PhantomData,
        })
    }

    /// The raw `switch_plc_state_t` pointer for escape-hatch FFI.
    #[inline]
    pub(crate) fn as_ptr(&self) -> *mut sys::switch_plc_state_t {
        self.raw.as_ptr()
    }

    /// Feeds a received audio frame into the PLC.
    ///
    /// `samples` is a slice of signed 16-bit PCM samples (`int16_t`). The buffer is mutated in
    /// place — spandsp's `switch_plc_rx` takes a mutable pointer and may overwrite samples with
    /// concealed audio derived from the reception history — so callers must not alias or share
    /// the buffer across threads during the call. Returns the number of samples processed.
    pub fn rx(&self, samples: &mut [i16]) -> i32 {
        if samples.is_empty() {
            return 0;
        }
        // SAFETY: `self.raw` is a live, owned PLC. `samples.as_mut_ptr()`/`len()` describe a
        // valid mutable `int16_t` buffer for the duration of the call. spandsp writes at most
        // `len` samples back into `amp`.
        let n = unsafe {
            sys::switch_plc_rx(
                self.raw.as_ptr(),
                samples.as_mut_ptr(),
                samples.len() as std::os::raw::c_int,
            )
        };
        n as i32
    }

    /// Generates `samples.len()` samples of concealment audio into `samples`.
    ///
    /// Call this when a packet is known to be lost: spandsp synthesizes replacement audio from
    /// the recent reception history maintained by prior [`rx`](Self::rx) calls and writes it into
    /// `samples`. The buffer is fully overwritten, so callers must not rely on any prior contents.
    /// Returns the number of samples written.
    pub fn fillin(&self, samples: &mut [i16]) -> i32 {
        if samples.is_empty() {
            return 0;
        }
        // SAFETY: `self.raw` is a live, owned PLC. `samples.as_mut_ptr()`/`len()` describe a
        // valid mutable `int16_t` buffer for the duration of the call. spandsp writes `len`
        // synthesized samples into `amp`.
        let n = unsafe {
            sys::switch_plc_fillin(
                self.raw.as_ptr(),
                samples.as_mut_ptr(),
                samples.len() as std::os::raw::c_int,
            )
        };
        n as i32
    }
}

impl Default for Plc {
    #[inline]
    fn default() -> Self {
        // `new` only fails on spandsp allocation failure, which cannot be recovered from a
        // `Default` context; propagate it as a panic so misuse surfaces immediately rather than
        // as a silently broken `Plc`.
        Self::new().expect("switch_plc_init returned null")
    }
}

impl Drop for Plc {
    fn drop(&mut self) {
        // SAFETY: `self.raw` owns exactly one `switch_plc_state_t` allocated by `switch_plc_init`
        // in `new`. `switch_plc_free` releases spandsp's heap allocation; the handle is not
        // shared — this `Plc` owns it exclusively — so a single free is correct.
        let rc = unsafe { sys::switch_plc_free(self.raw.as_ptr()) };
        if rc != 0 {
            log_error("plc", "switch_plc_free returned non-zero status");
        }
    }
}

impl std::fmt::Debug for Plc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Plc").field("ptr", &self.raw).finish()
    }
}

#[cfg(all(test, feature = "live_fs"))]
mod tests {
    use super::*;

    #[test]
    fn empty_slices_short_circuit_without_ffi() {
        // The empty-buffer fast paths must not touch the (uninitialized) handle, so they are
        // safe to exercise without a live PLC. They guard against passing a null `amp`/zero `len`
        // into spandsp when callers hand in an empty frame.
        let plc = Plc {
            raw: NonNull::dangling(),
            _marker: PhantomData,
        };
        assert_eq!(plc.rx(&mut []), 0);
        assert_eq!(plc.fillin(&mut []), 0);
        // Prevent Drop from freeing the dangling pointer.
        std::mem::forget(plc);
    }
}
