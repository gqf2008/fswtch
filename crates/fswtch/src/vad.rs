//! Voice Activity Detection (VAD) — a small, owned wrapper over FreeSWITCH's `switch_vad_t`.
//!
//! FreeSWITCH's VAD detects speech in 16-bit PCM audio frames, emitting state transitions
//! (`StartTalking`, `Talking`, `StopTalking`) as audio is fed in via [`Vad::process`]. This
//! module exposes that API without exposing the caller to `unsafe`.
//!
//! The handle is owned: [`Vad::new`] allocates the underlying `switch_vad_t` and [`Drop`]
//! calls `switch_vad_destroy`, so a `Vad` cleans up after itself like any other RAII guard.

use std::ffi::CString;
use std::fmt;
use std::marker::PhantomData;
use std::ptr::NonNull;

use crate::{Result, SwitchError, cstring, sys};

/// The outcome of feeding a frame of audio into [`Vad::process`], or the VAD's current state.
///
/// Wraps FreeSWITCH's `switch_vad_state_t`. The values mirror the `SWITCH_VAD_STATE_*`
/// constants exposed by `fswtch-sys`.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct VadState(pub sys::switch_vad_state_t);

impl VadState {
    /// The VAD has no transition to report yet (idle / between events).
    pub const NONE: VadState = VadState(sys::switch_vad_state_t_SWITCH_VAD_STATE_NONE);

    /// Speech just began in the most recently processed frame.
    pub const START_TALKING: VadState =
        VadState(sys::switch_vad_state_t_SWITCH_VAD_STATE_START_TALKING);

    /// Speech is ongoing (continues from a prior `START_TALKING`).
    pub const TALKING: VadState = VadState(sys::switch_vad_state_t_SWITCH_VAD_STATE_TALKING);

    /// Speech just ended in the most recently processed frame.
    pub const STOP_TALKING: VadState =
        VadState(sys::switch_vad_state_t_SWITCH_VAD_STATE_STOP_TALKING);

    /// The VAD hit an internal error.
    pub const ERROR: VadState = VadState(sys::switch_vad_state_t_SWITCH_VAD_STATE_ERROR);

    /// Wraps a raw `switch_vad_state_t` returned from FFI.
    #[inline]
    pub const fn from_raw(state: sys::switch_vad_state_t) -> Self {
        Self(state)
    }

    /// The underlying integer value.
    #[inline]
    pub const fn as_u32(self) -> u32 {
        self.0
    }

    /// `true` when this state marks the start of speech (`START_TALKING` or `TALKING`).
    #[inline]
    pub fn is_talking(self) -> bool {
        matches!(self, Self::START_TALKING | Self::TALKING)
    }

    /// The canonical name FreeSWITCH uses for this state (e.g. `"TALKING"`).
    ///
    /// Returns `None` if the underlying `switch_vad_state2str` returns a null pointer for an
    /// unknown value.
    pub fn name(self) -> Option<&'static str> {
        // SAFETY: `switch_vad_state2str` is a pure lookup over the `SWITCH_VAD_STATE_*`
        // constants; it returns either a static string literal or NULL for an unknown value.
        let ptr = unsafe { sys::switch_vad_state2str(self.0) };
        if ptr.is_null() {
            return None;
        }
        // SAFETY: a non-null pointer here points at a static null-terminated string literal
        // owned by the FreeSWITCH binary; it is valid for the program's lifetime.
        unsafe { crate::borrowed_cstr_to_str(ptr) }
    }
}

impl fmt::Display for VadState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.name() {
            Some(name) => f.write_str(name),
            None => write!(f, "VadState({})", self.0),
        }
    }
}

/// An owned voice-activity detector.
///
/// Allocated with [`Vad::new`] and destroyed on [`Drop`] via `switch_vad_destroy`. Feed PCM
/// frames in with [`process`](Self::process) and read the resulting [`VadState`].
pub struct Vad {
    raw: NonNull<sys::switch_vad_t>,
    // Not thread-safe; `process` mutates the VAD's internal state through `&self`.
    _marker: PhantomData<*const ()>,
}

impl Vad {
    /// Creates a new VAD for the given `sample_rate` (Hz) and `channels`.
    ///
    /// `sample_rate` is typically `8000`, `16000`, `32000`, or `48000`; `channels` is usually
    /// `1` (mono). Returns [`crate::SwitchError`](`crate::GENERR`) if allocation fails or the
    /// arguments are out of range.
    pub fn new(sample_rate: i32, channels: i32) -> Result<Self> {
        // SAFETY: `switch_vad_init` is a plain allocator taking two ints; passing arbitrary
        // integers is sound (it returns NULL for invalid arguments).
        let raw = unsafe { sys::switch_vad_init(sample_rate as _, channels as _) };
        NonNull::new(raw)
            .map(|raw| Self {
                raw,
                _marker: PhantomData,
            })
            .ok_or(SwitchError(crate::GENERR))
    }

    /// The raw `switch_vad_t` pointer. Useful as an escape hatch for direct FFI.
    #[inline]
    pub fn as_ptr(&self) -> *mut sys::switch_vad_t {
        self.raw.as_ptr()
    }

    /// Feeds one PCM frame to the VAD and returns the resulting state transition.
    ///
    /// `pcm` is a slice of signed 16-bit samples (`int16_t`). `samples` passed to the FFI is
    /// `pcm.len()`. The slice is mutated in place — FreeSWITCH's `switch_vad_process` takes a
    /// mutable pointer and may read/write the buffer — so callers should not share it across
    /// threads during the call.
    pub fn process(&self, pcm: &mut [i16]) -> VadState {
        // SAFETY: `self.raw` is a live, owned VAD. `pcm.as_mut_ptr()`/`len()` describe a valid
        // mutable buffer for the duration of the call.
        let state = unsafe {
            sys::switch_vad_process(
                self.raw.as_ptr(),
                pcm.as_mut_ptr(),
                pcm.len() as sys::switch_vad_state_t,
            )
        };
        VadState::from_raw(state)
    }

    /// Resets the VAD to its initial state, clearing any remembered speech/silence history.
    pub fn reset(&self) {
        // SAFETY: `self.raw` is a live VAD.
        unsafe { sys::switch_vad_reset(self.raw.as_ptr()) };
    }

    /// The VAD's current (most recently produced) state without feeding new audio.
    pub fn state(&self) -> VadState {
        // SAFETY: `self.raw` is a live VAD.
        let state = unsafe { sys::switch_vad_get_state(self.raw.as_ptr()) };
        VadState::from_raw(state)
    }

    /// Sets the VAD sensitivity mode.
    ///
    /// Valid modes (per `switch_vad.h`):
    /// - `-1`: disable fvad, use the native detector
    /// - `0`: quality
    /// - `1`: low bitrate
    /// - `2`: aggressive
    /// - `3`: very aggressive
    ///
    /// Returns [`crate::SwitchError`](`crate::GENERR`) on failure (non-zero return).
    pub fn set_mode(&self, mode: i32) -> Result<()> {
        // SAFETY: `self.raw` is a live VAD; `mode` is a plain integer.
        let rc = unsafe { sys::switch_vad_set_mode(self.raw.as_ptr(), mode as _) };
        if rc == 0 {
            Ok(())
        } else {
            Err(SwitchError(crate::GENERR))
        }
    }

    /// Sets a named VAD parameter to an integer value.
    ///
    /// `key` is a NUL-free C string (interior NULs map to [`crate::SwitchError`](`crate::GENERR`)).
    /// The value type is `int` in the FreeSWITCH API (`switch_vad_set_param`), so this takes an
    /// `i32` rather than a float.
    pub fn set_param(&self, key: impl AsRef<str>, val: i32) -> Result<()> {
        let key: CString = cstring(key)?;
        // SAFETY: `self.raw` is a live VAD; `key` is a valid null-terminated C string for the
        // duration of the call.
        unsafe { sys::switch_vad_set_param(self.raw.as_ptr(), key.as_ptr(), val as _) };
        // `switch_vad_set_param` returns void, so there is no status to map.
        Ok(())
    }
}

impl Drop for Vad {
    fn drop(&mut self) {
        // SAFETY: `self.raw` owns exactly one `switch_vad_t`, and `switch_vad_destroy` takes the
        // pointer by reference (`*mut *mut`) so it can NULL it out; the box is not otherwise
        // touched after this point.
        let mut ptr = self.raw.as_ptr();
        unsafe { sys::switch_vad_destroy(&mut ptr) };
    }
}

impl fmt::Debug for Vad {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Vad")
            .field("ptr", &self.raw)
            .field("state", &self.state())
            .finish()
    }
}
