//! speexdsp — echo cancellation, preprocessing (denoise/AGC/VAD), resampling.
//!
//! Direct wrappers over libspeexdsp (`speex_*`), not FreeSWITCH's `switch_*` layer. Each type
//! is an RAII guard: `Drop` calls the matching `*_destroy`, so no manual cleanup needed.
//!
//! Not thread-safe: all three state types have internal mutable state accessed through `&self`
//! (mirroring the C API's `*const`-but-mutates convention). Marked `!Send + !Sync`.

use std::ffi::c_void;
use std::marker::PhantomData;
use std::ptr::NonNull;

use crate::{GENERR, Result, SwitchError, sys};

// ── SpeexEcho (acoustic echo cancellation) ────────────────────────────────

/// An acoustic echo canceller backed by `speex_echo_state`.
///
/// Allocate with [`new`](Self::new), feed each frame's near-end (`rec`) and far-end (`play`)
/// reference to [`cancellation`](Self::cancellation), and the output is echo-cancelled audio.
/// Couple with [`SpeexPreprocess::set_echo_state`] for residual echo suppression.
pub struct SpeexEcho {
    raw: NonNull<sys::SpeexEchoState>,
    _marker: PhantomData<*const ()>,
}

impl SpeexEcho {
    /// Creates an echo canceller for `frame_size` samples per frame and a `filter_length`
    /// (echo tail) in samples. Returns `None` on allocation failure.
    pub fn new(frame_size: i32, filter_length: i32) -> Option<Self> {
        // SAFETY: `frame_size`/`filter_length` are plain ints; returns null on failure.
        let raw = unsafe { sys::speex_echo_state_init(frame_size, filter_length) };
        NonNull::new(raw).map(|raw| Self {
            raw,
            _marker: PhantomData,
        })
    }

    /// Runs echo cancellation: `rec` is the near-end (mic), `play` the far-end (reference/playout),
    /// `out` receives the cleaned signal. All three slices must be at least `frame_size` long.
    pub fn cancellation(&self, rec: &[i16], play: &[i16], out: &mut [i16]) {
        // SAFETY: `self.raw` live; `rec`/`play`/`out` are valid i16 slices for the call.
        unsafe {
            sys::speex_echo_cancellation(
                self.raw.as_ptr(),
                rec.as_ptr(),
                play.as_ptr(),
                out.as_mut_ptr(),
            );
        }
    }

    /// Low-level control (`speex_echo_ctl`). `request` is a `SPEEX_ECHO_*` constant; `ptr` points
    /// to the request's value (int, struct, etc.). Returns the result code.
    pub fn ctl(&self, request: u32, ptr: *mut c_void) -> i32 {
        // SAFETY: `self.raw` live; `ptr` valid per caller contract for the given `request`.
        unsafe { sys::speex_echo_ctl(self.raw.as_ptr(), request as i32, ptr) }
    }

    /// Sets the sampling rate (Hz) of the echo canceller.
    pub fn set_sampling_rate(&self, rate: i32) {
        let mut rate = rate;
        // SAFETY: `&mut rate` is a valid `*mut c_void` for this request.
        self.ctl(
            sys::SPEEX_ECHO_SET_SAMPLING_RATE,
            &mut rate as *mut _ as *mut c_void,
        );
    }

    /// The frame size this echo canceller was initialized with.
    pub fn frame_size(&self) -> i32 {
        let mut size: i32 = 0;
        // SAFETY: `&mut size` valid out-param for SPEEX_ECHO_GET_FRAME_SIZE.
        self.ctl(
            sys::SPEEX_ECHO_GET_FRAME_SIZE,
            &mut size as *mut _ as *mut c_void,
        );
        size
    }

    /// Raw pointer for coupling with `SpeexPreprocess::set_echo_state`.
    pub(crate) fn as_ptr(&self) -> *mut sys::SpeexEchoState {
        self.raw.as_ptr()
    }
}

impl Drop for SpeexEcho {
    fn drop(&mut self) {
        // SAFETY: `self.raw` owns exactly one echo state; destroy frees it.
        unsafe { sys::speex_echo_state_destroy(self.raw.as_ptr()) };
    }
}

// ── SpeexPreprocess (denoise / AGC / VAD) ────────────────────────────────

/// A preprocessor backed by `speex_preprocess_state`.
///
/// Performs denoise, AGC, VAD, and residual echo suppression on each frame. Allocate with
/// [`new`](Self::new), configure via [`set_denoise`](Self::set_denoise) /
/// [`set_agc`](Self::set_agc) / [`set_noise_suppress`](Self::set_noise_suppress), then call
/// [`run`](Self::run) on each frame.
pub struct SpeexPreprocess {
    raw: NonNull<sys::SpeexPreprocessState>,
    _marker: PhantomData<*const ()>,
}

impl SpeexPreprocess {
    /// Creates a preprocessor for `frame_size` samples per frame at `sample_rate` Hz.
    pub fn new(frame_size: i32, sample_rate: i32) -> Option<Self> {
        // SAFETY: plain ints; returns null on failure.
        let raw = unsafe { sys::speex_preprocess_state_init(frame_size, sample_rate) };
        NonNull::new(raw).map(|raw| Self {
            raw,
            _marker: PhantomData,
        })
    }

    /// Runs the preprocessor on `frame` (in-place). Returns `true` if voice was detected (VAD),
    /// `false` for silence.
    pub fn run(&self, frame: &mut [i16]) -> bool {
        // SAFETY: `self.raw` live; `frame` is a valid mutable i16 slice for the call.
        unsafe { sys::speex_preprocess_run(self.raw.as_ptr(), frame.as_mut_ptr()) != 0 }
    }

    /// Low-level control (`speex_preprocess_ctl`).
    pub fn ctl(&self, request: u32, ptr: *mut c_void) -> i32 {
        // SAFETY: `self.raw` live; `ptr` valid per caller.
        unsafe { sys::speex_preprocess_ctl(self.raw.as_ptr(), request as i32, ptr) }
    }

    /// Enables/disables denoise (spectral noise subtraction).
    pub fn set_denoise(&self, enable: bool) {
        let val: i32 = enable.into();
        self.ctl(
            sys::SPEEX_PREPROCESS_SET_DENOISE,
            &val as *const _ as *mut c_void,
        );
    }

    /// Enables/disables automatic gain control.
    pub fn set_agc(&self, enable: bool) {
        let val: i32 = enable.into();
        self.ctl(
            sys::SPEEX_PREPROCESS_SET_AGC,
            &val as *const _ as *mut c_void,
        );
    }

    /// Sets the target AGC level (dB, typically ~8000 for 8 kHz or ~30000 for 16 kHz).
    pub fn set_agc_level(&self, level: i32) {
        self.ctl(
            sys::SPEEX_PREPROCESS_SET_AGC_LEVEL,
            &level as *const _ as *mut c_void,
        );
    }

    /// Sets the noise suppression floor (negative dB, e.g. `-30`).
    pub fn set_noise_suppress(&self, db: i32) {
        self.ctl(
            sys::SPEEX_PREPROCESS_SET_NOISE_SUPPRESS,
            &db as *const _ as *mut c_void,
        );
    }

    /// Enables/disables VAD (voice activity detection) in the preprocessor.
    pub fn set_vad(&self, enable: bool) {
        let val: i32 = enable.into();
        self.ctl(
            sys::SPEEX_PREPROCESS_SET_VAD,
            &val as *const _ as *mut c_void,
        );
    }

    /// Couples residual echo suppression with an echo canceller — the preprocessor uses the
    /// echo state to suppress residual echo that the AEC didn't fully remove.
    ///
    /// **The `SpeexEcho` must outlive this `SpeexPreprocess`.** The preprocessor stores a raw
    /// pointer to the echo state; if the echo is dropped first, subsequent `run()` calls read a
    /// dangling pointer (use-after-free).
    pub fn set_echo_state(&self, echo: &SpeexEcho) {
        // SAFETY: both states live; the echo state pointer is borrowed for the coupling.
        self.ctl(
            sys::SPEEX_PREPROCESS_SET_ECHO_STATE,
            echo.as_ptr() as *mut c_void,
        );
    }

    /// Sets the residual echo suppression floor (negative dB).
    pub fn set_echo_suppress(&self, db: i32) {
        self.ctl(
            sys::SPEEX_PREPROCESS_SET_ECHO_SUPPRESS,
            &db as *const _ as *mut c_void,
        );
    }
}

impl Drop for SpeexPreprocess {
    fn drop(&mut self) {
        // SAFETY: `self.raw` owns exactly one preprocess state.
        unsafe { sys::speex_preprocess_state_destroy(self.raw.as_ptr()) };
    }
}

// ── SpeexResampler (sample rate conversion) ───────────────────────────────

/// A resampler backed by `speex_resampler_state`.
///
/// Allocate with [`new`](Self::new) (specifying in/out rates + quality 0-10), then call
/// [`process_int`](Self::process_int) on interleaved i16 audio. Quality 0 = fastest, 10 = best.
pub struct SpeexResampler {
    raw: NonNull<sys::SpeexResamplerState>,
    _marker: PhantomData<*const ()>,
}

impl SpeexResampler {
    /// Creates a resampler for `channels` channels, `in_rate` → `out_rate` (Hz), at `quality`
    /// (0-10). Returns `Err` on failure (invalid channels/rate/quality).
    pub fn new(channels: u32, in_rate: u32, out_rate: u32, quality: i32) -> Result<Self> {
        let mut err: i32 = 0;
        // SAFETY: plain uints + int; `&mut err` valid out-param; returns null on error.
        let raw =
            unsafe { sys::speex_resampler_init(channels, in_rate, out_rate, quality, &mut err) };
        if err != 0 {
            return Err(SwitchError(GENERR));
        }
        NonNull::new(raw)
            .map(|raw| Self {
                raw,
                _marker: PhantomData,
            })
            .ok_or(SwitchError(GENERR))
    }

    /// Resamples interleaved i16 audio: `input` → `output`. Returns `(status, in_used,
    /// out_generated)` — the speexdsp status code (0 = success, negative = error), the number of
    /// input samples consumed, and output samples produced.
    pub fn process_int(&self, channel: u32, input: &[i16], output: &mut [i16]) -> (i32, u32, u32) {
        let mut in_len = input.len() as u32;
        let mut out_len = output.len() as u32;
        // SAFETY: `self.raw` live; `input`/`output` valid i16 slices; `&mut in_len`/`&mut out_len`
        // are valid in/out params.
        let status = unsafe {
            sys::speex_resampler_process_int(
                self.raw.as_ptr(),
                channel,
                input.as_ptr(),
                &mut in_len,
                output.as_mut_ptr(),
                &mut out_len,
            )
        };
        (status, in_len, out_len)
    }

    /// Changes the in/out sample rates (Hz).
    pub fn set_rate(&self, in_rate: u32, out_rate: u32) -> i32 {
        // SAFETY: `self.raw` live; plain uints.
        unsafe { sys::speex_resampler_set_rate(self.raw.as_ptr(), in_rate, out_rate) }
    }
}

impl Drop for SpeexResampler {
    fn drop(&mut self) {
        // SAFETY: `self.raw` owns exactly one resampler state.
        unsafe { sys::speex_resampler_destroy(self.raw.as_ptr()) };
    }
}
