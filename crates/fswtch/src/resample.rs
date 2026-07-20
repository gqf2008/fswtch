//! Audio resampling, automatic gain control, and PCM format conversion.
//!
//! This module wraps FreeSWITCH's `switch_resample.h` interface. Three concerns live here:
//!
//! - [`Resample`] — an owned resampler handle that converts signed-linear (`i16`) audio between
//!   two sample rates. `switch_resample_process` reads from a caller-supplied input buffer and
//!   writes the resampled output into the handle's internal `to` buffer, so [`Resample::process`]
//!   returns a borrowed slice rather than mutating the input.
//! - [`Agc`] — an owned automatic-gain-control handle driven by [`AgcConfig`].
//! - The free functions convert between sample formats (`short`/`float`/`char`) and adjust
//!   signed-linear volume, plus a few buffer utilities.
//!
//! All public entry points are safe; the raw FreeSWITCH pointers never escape the wrappers.

use std::ffi::CString;
use std::marker::PhantomData;
use std::ptr::NonNull;

use crate::{GENERR, Result, SwitchError, cstring, status_to_result, sys};

/// Default resampler quality, matching FreeSWITCH's `SWITCH_RESAMPLE_QUALITY`.
pub const DEFAULT_QUALITY: i32 = 2;

/// A live audio resampler that converts `i16` (signed-linear) samples from one rate to another.
///
/// Owned wrapper around `switch_audio_resampler_t`. Resampling is *not* in-place: [`process`] reads
/// the input slice and writes into the handle's internal output buffer, returning a borrowed view
/// of the result that is valid until the next `process` call (or until the handle is dropped).
///
/// [`process`]: Resample::process
pub struct Resample {
    raw: NonNull<sys::switch_audio_resampler_t>,
    // FreeSWITCH's `switch_audio_resampler_t` stores from_rate/to_rate/channels and the live `to`
    // buffer capacity, but *not* the quality or the originally-requested to_size — so we cache
    // both here. They are read-only after construction (no setters), so plain fields suffice.
    quality: i32,
    to_size: u32,
    // Not thread-safe; `process` mutates the resampler's internal `to` buffer through `&self`.
    _marker: PhantomData<*const ()>,
}

impl Resample {
    /// Creates a new resampler converting `from_rate` -> `to_rate` for `channels` interleaved
    /// channels at the given `quality` (pass [`DEFAULT_QUALITY`] when in doubt).
    ///
    /// Note: although FreeSWITCH's `switch_resample_create` macro reads as if it takes a memory
    /// pool, the emitted `switch_resample_perform_create` does not — the resampler allocates its
    /// own internal output buffer during creation. No pool parameter is required.
    pub fn new(from_rate: u32, to_rate: u32, channels: u32, quality: i32) -> Result<Self> {
        // A generous initial output capacity; `switch_resample_process` grows `to` as needed.
        let to_size = calc_buffer_size(to_rate, from_rate, 8192).max(8192);
        let mut raw: *mut sys::switch_audio_resampler_t = std::ptr::null_mut();
        // SAFETY: `raw` is writable output storage; the file/func/line strings are static C strings
        // supplied for FreeSWITCH's diagnostic logging.
        let status = unsafe {
            sys::switch_resample_perform_create(
                &mut raw,
                from_rate,
                to_rate,
                to_size,
                quality,
                channels,
                c"fswtch-rs".as_ptr(),
                c"Resample::new".as_ptr(),
                line!() as _,
            )
        };
        status_to_result(status)?;
        // SAFETY: `switch_resample_perform_create` returned SUCCESS, so `raw` is a live handle.
        let raw = NonNull::new(raw).ok_or(SwitchError(GENERR))?;
        Ok(Self {
            raw,
            quality,
            to_size,
            _marker: PhantomData,
        })
    }

    /// Creates a resampler using [`DEFAULT_QUALITY`].
    pub fn with_default_quality(from_rate: u32, to_rate: u32, channels: u32) -> Result<Self> {
        Self::new(from_rate, to_rate, channels, DEFAULT_QUALITY)
    }

    /// The raw resampler pointer, for advanced use with the FreeSWITCH API.
    pub(crate) fn as_ptr(&self) -> *mut sys::switch_audio_resampler_t {
        self.raw.as_ptr()
    }

    /// The configured source sample rate, in Hz.
    pub fn from_rate(&self) -> u32 {
        // SAFETY: `self.raw` is a live resampler; reading a plain integer field is safe.
        unsafe { (*self.raw.as_ptr()).from_rate as u32 }
    }

    /// The configured destination sample rate, in Hz.
    pub fn to_rate(&self) -> u32 {
        // SAFETY: `self.raw` is a live resampler; reading a plain integer field is safe.
        unsafe { (*self.raw.as_ptr()).to_rate as u32 }
    }

    /// The number of interleaved channels the resampler was created for.
    pub fn channels(&self) -> u32 {
        // SAFETY: `self.raw` is a live resampler; reading a plain integer field is safe.
        unsafe { (*self.raw.as_ptr()).channels as u32 }
    }

    /// The resampler quality used at creation (0-10). FreeSWITCH's `switch_audio_resampler_t`
    /// does not retain the quality after creation, so this value is cached at construction time.
    pub fn quality(&self) -> i32 {
        self.quality
    }

    /// The target output buffer size (in samples) requested at creation. Note that
    /// [`process`](Self::process) may grow the internal `to` buffer beyond this; for the current
    /// allocated capacity see [`to_capacity`](Self::to_capacity).
    pub fn to_size(&self) -> u32 {
        self.to_size
    }

    /// The current allocated capacity (in samples) of the internal `to` output buffer. Unlike
    /// [`to_size`](Self::to_size) (the creation-time request), this reflects live growth by
    /// `switch_resample_process`.
    pub fn to_capacity(&self) -> u32 {
        // SAFETY: `self.raw` is a live resampler; reading a plain uint field is safe.
        unsafe { (*self.raw.as_ptr()).to_size }
    }

    /// Resamples `src` and returns a borrowed slice of the resampled output.
    ///
    /// The returned slice borrows the resampler's internal `to` buffer and is valid only while the
    /// borrow on `self` holds and until the next call to `process`. Each call overwrites the
    /// previous output.
    ///
    /// `src` is taken by exclusive reference because the underlying `switch_resample_process` C
    /// signature declares `int16_t *src` (non-const) — FreeSWITCH does not document whether it
    /// mutates the source, so an exclusive borrow is the sound choice.
    pub fn process(&self, src: &mut [i16]) -> &[i16] {
        let srclen = u32::try_from(src.len()).unwrap_or(u32::MAX);
        // SAFETY: `self.raw` is a live resampler; `src` is a valid readable buffer of `srclen`
        // samples. The call writes into the resampler's internal `to` buffer and returns the count
        // of samples written (never more than the allocated `to_size`).
        let out_len =
            unsafe { sys::switch_resample_process(self.raw.as_ptr(), src.as_mut_ptr(), srclen) };
        let out_len = out_len as usize;
        // SAFETY: `self.raw` is a live resampler; `to` is the internal output buffer and `out_len`
        // is the number of samples the call just wrote into it.
        let to_ptr = unsafe { (*self.raw.as_ptr()).to };
        if to_ptr.is_null() || out_len == 0 {
            return &[];
        }
        // SAFETY: `to_ptr` points to `out_len` consecutive valid `i16` samples owned by the
        // resampler and live for the duration of this borrow.
        unsafe { std::slice::from_raw_parts(to_ptr, out_len) }
    }
}

impl Drop for Resample {
    fn drop(&mut self) {
        let mut raw = self.raw.as_ptr();
        // SAFETY: `raw` is the owned handle; `switch_resample_destroy` frees it and nulls the
        // pointer. Called exactly once on drop.
        unsafe { sys::switch_resample_destroy(&mut raw) };
    }
}

// ────────────────────────────────────────────────────────────────────────────
// StreamingResample — streaming wrapper with an integer-ratio fast path.
// ────────────────────────────────────────────────────────────────────────────
//
// Rationale and path selection live on the struct rustdoc below to keep a single source of
// truth. In short: integer-ratio upsampling takes a stateless linear-interpolation fast path
// (speex's shared filter delay lines attenuate the signal ~14 dB on the vox-seat AEC chain);
// everything else delegates to the inner speex `Resample`; `process_into` writes into a
// caller-reused `Vec` for a zero-alloc 20 ms hot path.

/// Streaming resampler with an integer-ratio linear-interpolation fast path.
///
/// Wraps [`Resample`] for non-integer ratios and downsampling (where speex's anti-aliasing
/// filter is needed), and uses stateless linear interpolation for integer-ratio upsampling
/// (where speex's filter delay lines would attenuate and ring). Optimized for the real-time
/// media-bug calling pattern: one [`process_into`](Self::process_into) call per frame, writing
/// into a caller-owned reused `Vec` so the 20 ms hot path allocates nothing after warmup.
///
/// Mono only (channels = 1) — matches every current FreeSWITCH leg (G.711, Opus wideband in
/// the media bug). If you need multichannel, use [`Resample`] directly.
///
/// # When to use what
///
/// - [`StreamingResample`] — real-time media-bug path (integer-ratio upsample fast path +
///   zero-alloc `process_into`). FFI-backed (needs `live_fs` to link).
/// - [`Resample`] — the underlying thin speex wrapper; multichannel, or when you don't need
///   the fast path / reused-buffer API.
/// - [`crate::dsp::SampleRateConverter`] — a pure-Rust (rubato, no FreeSWITCH) streaming
///   resampler for non-media-bug paths (e.g. TTS). Prefer it when you have no `live_fs`.
///
/// # Quality
///
/// FreeSWITCH speex quality ranges 0 (fastest, lowest) to 10 (slowest, highest); pass
/// [`DEFAULT_QUALITY`] (= 2) when in doubt. Only consulted when the inner `Resample` is used
/// (non-integer-ratio / downsampling path); the integer-ratio fast path is quality-agnostic.
/// Out-of-range values are rejected at construction.
pub struct StreamingResample {
    inner: Resample,
    from_rate: u32,
    to_rate: u32,
    /// Integer upsample factor (e.g. 2 for 8 kHz → 16 kHz) when the ratio is an integer > 1,
    /// selecting the stateless linear-interpolation fast path; `0` otherwise (non-integer
    /// ratio, downsampling, or same-rate passthrough → inner speex / copy). `quality` is read
    /// back from `inner.quality()` on `reset`, so it isn't duplicated here.
    upsample_factor: usize,
    /// Lazily-initialized mutable scratch for [`Resample::process`] (which takes `&mut [i16]`).
    /// Allocated only on the first speex-path call, so integer-ratio (fast-path) instances — the
    /// primary use case — never pay for it. Grows if a larger frame arrives.
    scratch_in: Option<Vec<i16>>,
}

impl StreamingResample {
    /// Create a mono resampler converting `from_rate` → `to_rate` at the given speex `quality`.
    ///
    /// Construction allocates the inner speex handle regardless of the ratio (so `Drop` is
    /// balanced and callers can [`reset`](Self::reset) into either path). The integer-ratio
    /// upsampling fast path then bypasses speex entirely at process time.
    ///
    /// `quality` is validated here (0-10) **before** the speex handle is created, so an
    /// out-of-range value fails fast without touching FreeSWITCH state. Same for `from_rate` /
    /// `to_rate` (must be non-zero).
    pub fn new(from_rate: u32, to_rate: u32, quality: i32) -> std::result::Result<Self, String> {
        // Validate cheap Rust invariants BEFORE constructing the speex handle — fail fast keeps
        // callers off the FFI on bad input and lets the validation tests run without `live_fs`.
        if from_rate == 0 || to_rate == 0 {
            return Err(format!(
                "Invalid sample rates: from={}, to={}",
                from_rate, to_rate
            ));
        }
        if !(0..=10).contains(&quality) {
            return Err(format!("Invalid quality {}: must be 0-10", quality));
        }
        let inner = Resample::new(from_rate, to_rate, 1, quality)
            .map_err(|e| format!("switch_resample_perform_create failed: {e:?}"))?;
        // Integer-ratio detection via integer arithmetic — no f64 precision concerns. `from_rate`
        // is non-zero (validated above), so the division is safe. `upsample_factor` holds the
        // exact integer ratio only when it exceeds 1 (fast path); `0` otherwise (non-integer
        // ratio, downsampling, or same-rate — handled by the inner speex path / passthrough).
        let ratio = to_rate / from_rate;
        let upsample_factor = if to_rate.is_multiple_of(from_rate) && ratio > 1 {
            ratio as usize
        } else {
            0
        };
        Ok(Self {
            inner,
            from_rate,
            to_rate,
            upsample_factor,
            // `scratch_in` is allocated lazily on the first speex-path call; fast-path instances
            // (the primary use case) never need it.
            scratch_in: None,
        })
    }

    /// Process `input` into the caller-owned `out` buffer (zero per-frame alloc after warmup).
    ///
    /// `out` is cleared and refilled with the resampled output. Callers reuse a pre-allocated
    /// `Vec` across frames so the 20 ms media path never heap-allocates after the first frame.
    ///
    /// Selects the path chosen at construction: integer-ratio upsampling runs the stateless
    /// linear-interpolation fast path (no speex filter delay or shared-delay attenuation);
    /// everything else delegates to the inner [`Resample`] (speex with anti-aliasing).
    ///
    /// On the fast path the trailing input sample of each call is *held* (zero slope) for the
    /// final `n-1` output slots, so every call emits exactly `input.len() * n` samples and the
    /// output rate stays exact across streamed frames. The cost is a brief hold (`n` flat
    /// samples) at each frame tail — preferred over extrapolation (can overshoot) and over
    /// truncating the tail (would under-produce and skew the rate frame-to-frame).
    pub fn process_into(&mut self, input: &[i16], out: &mut Vec<i16>) {
        out.clear();
        if input.is_empty() {
            return;
        }

        // Same-rate passthrough: speex would only add filter delay/attenuation for no benefit.
        if self.from_rate == self.to_rate {
            out.extend_from_slice(input);
            return;
        }

        // Integer-ratio upsampling: linear interpolation. See the module docs for why speex is
        // avoided on this path. `upsample_factor > 1` is the fast-path gate (0 otherwise).
        if self.upsample_factor > 1 {
            let n = self.upsample_factor;
            // Reserve the exact output length. `saturating_mul` only avoids the integer panic on
            // overflow — a pathological overflow still aborts on the reserve; real 10-20 ms
            // frames never approach it.
            out.reserve(input.len().saturating_mul(n));
            // Hoist the constant reciprocal out of the inner loop (one f32 div/frame vs. per
            // interpolated sample); `t = j * inv_n` is the per-sample phase.
            let inv_n = 1.0f32 / n as f32;
            for (i, &cur) in input.iter().enumerate() {
                out.push(cur);
                // Interpolate towards the next sample. At the tail (last input sample) hold the
                // last value (zero slope) rather than extrapolate — see `process_into` docs for
                // why the tail is held so output length stays exactly len * n.
                let next = if i + 1 < input.len() {
                    input[i + 1]
                } else {
                    cur
                };
                let cur_f = cur as f32;
                let slope = next as f32 - cur_f;
                for j in 1..n {
                    let t = j as f32 * inv_n;
                    out.push((cur_f + slope * t) as i16);
                }
            }
            return;
        }

        // Non-integer ratio or downsampling: speex via the inner Resample. `process` borrows the
        // input mutably and returns a borrow into the resampler's internal `to` buffer; copy into
        // the caller's `out` so the borrow is released before the next call. `scratch_in` is
        // allocated lazily — fast-path instances never reach here, so they never pay for it.
        let scratch = self
            .scratch_in
            .get_or_insert_with(|| Vec::with_capacity(input.len()));
        scratch.clear();
        scratch.extend_from_slice(input);
        // Pre-size `out` so the first (or a larger-than-seen) frame doesn't realloc — matches
        // the fast path's reserve, keeping "zero-alloc after warmup" symmetric across paths.
        out.reserve(calc_buffer_size(self.to_rate, self.from_rate, input.len() as u32) as usize);
        let borrowed = self.inner.process(scratch);
        out.extend_from_slice(borrowed);
    }

    /// Convenience wrapper that owns the output `Vec`. **Allocates** — intended for tests and
    /// one-shot diagnostics, NOT the media hot path. Hot-path callers use
    /// [`process_into`](Self::process_into) with a reused buffer.
    #[cfg(test)]
    fn process(&mut self, input: &[i16]) -> Vec<i16> {
        let mut out = Vec::new();
        self.process_into(input, &mut out);
        out
    }

    /// The configured source sample rate, in Hz.
    pub fn from_rate(&self) -> u32 {
        self.from_rate
    }

    /// The configured destination sample rate, in Hz.
    pub fn to_rate(&self) -> u32 {
        self.to_rate
    }

    /// `true` when this instance takes the integer-ratio linear-interpolation fast path.
    pub fn is_integer_upsample(&self) -> bool {
        self.upsample_factor > 1
    }

    /// Reset internal filter state (delay lines). Recreates the inner handle at the same
    /// `quality` (read back from `inner.quality()`), since FreeSWITCH's C API has no native
    /// flush/reset.
    ///
    /// Intended for barge-in / segment boundaries where leftover speex state would otherwise
    /// leak the previous segment's tail into the next. On the integer-ratio fast path this is a
    /// no-op (linear interpolation is stateless — there are no delay lines to clear), so the
    /// speex handle is *not* recreated there. If recreation fails on the speex path the old
    /// handle is preserved (subsequent [`process_into`](Self::process_into) calls still work)
    /// and a warning is logged.
    pub fn reset(&mut self) {
        // Fast path is stateless — nothing to reset, and recreating the speex handle would be
        // wasted FFI work on a handle this instance never reads.
        if self.upsample_factor > 1 {
            return;
        }
        match Resample::new(self.from_rate, self.to_rate, 1, self.inner.quality()) {
            Ok(new) => self.inner = new,
            Err(e) => crate::log_warning(
                "fswtch",
                format!("StreamingResample::reset failed ({e:?}); keeping old handle"),
            ),
        }
    }
}

/// Configuration for [`Agc::new`].
///
/// FreeSWITCH's automatic gain control is parameterized by five values; this builder keeps the
/// construction call site readable. All fields default to `0` (use [`AgcConfig::default`] and fill
/// in the values you care about).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AgcConfig {
    /// Rolling average energy level the AGC targets.
    pub energy_avg: u32,
    /// Energy floor below which the AGC stops boosting.
    pub low_energy_point: u32,
    /// Headroom between signal and noise floors.
    pub margin: u32,
    /// How aggressively the gain is adjusted each period.
    pub change_factor: u32,
    /// Number of samples per adjustment period.
    pub period_len: u32,
}

impl AgcConfig {
    /// Sets [`AgcConfig::energy_avg`].
    pub fn energy_avg(mut self, energy_avg: u32) -> Self {
        self.energy_avg = energy_avg;
        self
    }

    /// Sets [`AgcConfig::low_energy_point`].
    pub fn low_energy_point(mut self, low_energy_point: u32) -> Self {
        self.low_energy_point = low_energy_point;
        self
    }

    /// Sets [`AgcConfig::margin`].
    pub fn margin(mut self, margin: u32) -> Self {
        self.margin = margin;
        self
    }

    /// Sets [`AgcConfig::change_factor`].
    pub fn change_factor(mut self, change_factor: u32) -> Self {
        self.change_factor = change_factor;
        self
    }

    /// Sets [`AgcConfig::period_len`].
    pub fn period_len(mut self, period_len: u32) -> Self {
        self.period_len = period_len;
        self
    }
}

/// An owned automatic-gain-control handle.
///
/// Construct with [`Agc::new`] (or [`Agc::with_config`]) and drive with [`Agc::feed`], which
/// applies gain in place to the supplied `i16` sample buffer. The handle is freed on drop.
pub struct Agc {
    raw: NonNull<sys::switch_agc_t>,
    /// Owned copies of tokens set via [`Agc::set_token`]. `switch_agc_set_token` is not documented
    /// to copy its argument, so we retain the bytes here for the handle's lifetime to avoid a
    /// use-after-free if the AGC stores the pointer.
    tokens: Vec<CString>,
    // Not thread-safe; `feed` mutates C state through `&self`.
    _marker: PhantomData<*const ()>,
}

impl Agc {
    /// Creates a new AGC with the given configuration.
    pub fn with_config(config: AgcConfig) -> Result<Self> {
        let mut raw: *mut sys::switch_agc_t = std::ptr::null_mut();
        // SAFETY: `raw` is writable output storage.
        let status = unsafe {
            sys::switch_agc_create(
                &mut raw,
                config.energy_avg,
                config.low_energy_point,
                config.margin,
                config.change_factor,
                config.period_len,
            )
        };
        status_to_result(status)?;
        // SAFETY: `switch_agc_create` returned SUCCESS, so `raw` is a live handle.
        let raw = NonNull::new(raw).ok_or(SwitchError(GENERR))?;
        Ok(Self {
            raw,
            tokens: Vec::new(),
            _marker: PhantomData,
        })
    }

    /// Convenience constructor passing individual parameters (equivalent to
    /// `Agc::with_config(AgcConfig { ... })`).
    pub fn new(
        energy_avg: u32,
        low_energy_point: u32,
        margin: u32,
        change_factor: u32,
        period_len: u32,
    ) -> Result<Self> {
        Self::with_config(AgcConfig {
            energy_avg,
            low_energy_point,
            margin,
            change_factor,
            period_len,
        })
    }

    /// The raw AGC pointer, for advanced use with the FreeSWITCH API.
    pub(crate) fn as_ptr(&self) -> *mut sys::switch_agc_t {
        self.raw.as_ptr()
    }

    /// Replaces the AGC's tuning parameters in one call.
    pub fn set(
        &self,
        energy_avg: u32,
        low_energy_point: u32,
        margin: u32,
        change_factor: u32,
        period_len: u32,
    ) {
        // SAFETY: `self.raw` is a live AGC handle.
        unsafe {
            sys::switch_agc_set(
                self.raw.as_ptr(),
                energy_avg,
                low_energy_point,
                margin,
                change_factor,
                period_len,
            )
        };
    }

    /// Sets the target rolling average energy level.
    pub fn set_energy_avg(&self, energy_avg: u32) {
        // SAFETY: `self.raw` is a live AGC handle.
        unsafe { sys::switch_agc_set_energy_avg(self.raw.as_ptr(), energy_avg) };
    }

    /// Sets the energy floor below which the AGC stops boosting.
    pub fn set_energy_low(&self, low_energy_point: u32) {
        // SAFETY: `self.raw` is a live AGC handle.
        unsafe { sys::switch_agc_set_energy_low(self.raw.as_ptr(), low_energy_point) };
    }

    /// Sets an opaque token on the AGC (for module-private bookkeeping).
    ///
    /// `switch_agc_set_token` is not documented to copy its argument, so the bytes are retained by
    /// this [`Agc`] (in `self.tokens`) for the handle's lifetime and the stored pointer stays valid.
    pub fn set_token(&mut self, token: impl AsRef<str>) -> Result<()> {
        let token = cstring(token)?;
        // SAFETY: `self.raw` is a live AGC handle; `token` is a valid C string whose storage is
        // owned by `self.tokens` for the AGC's lifetime.
        unsafe { sys::switch_agc_set_token(self.raw.as_ptr(), token.as_ptr()) };
        self.tokens.push(token);
        Ok(())
    }

    /// Applies automatic gain to `samples` (in place) for `channels` interleaved channels.
    ///
    /// Returns `Ok(())` on success. The buffer is mutated in place even on error.
    pub fn feed(&self, samples: &mut [i16], channels: u32) -> Result<()> {
        let count = u32::try_from(samples.len()).unwrap_or(u32::MAX);
        // SAFETY: `self.raw` is a live AGC handle; `samples` is a writable buffer of `count`
        // samples for `channels` interleaved channels.
        let status = unsafe {
            sys::switch_agc_feed(self.raw.as_ptr(), samples.as_mut_ptr(), count, channels)
        };
        status_to_result(status)
    }
}

impl Drop for Agc {
    fn drop(&mut self) {
        let mut raw = self.raw.as_ptr();
        // SAFETY: `raw` is the owned handle; `switch_agc_destroy` frees it and nulls the pointer.
        // Called exactly once on drop.
        unsafe { sys::switch_agc_destroy(&mut raw) };
    }
}

/// Estimates the output buffer size needed to resample `srclen` samples from `from_rate` to
/// `to_rate`, mirroring FreeSWITCH's `switch_resample_calc_buffer_size` macro.
pub fn calc_buffer_size(to_rate: u32, from_rate: u32, srclen: u32) -> u32 {
    if from_rate == 0 {
        return 0;
    }
    ((to_rate as f64 / from_rate as f64) * srclen as f64) as u32
}

/// Converts an array of `i16` (short) samples into `f32` (float) samples.
///
/// FreeSWITCH scales each sample from the signed-16 range into `[-1.0, 1.0]`. The returned `Vec`
/// has the same length as `src`.
pub fn short_to_float(src: &[i16]) -> Vec<f32> {
    if src.is_empty() {
        return Vec::new();
    }
    let len = i32::try_from(src.len()).unwrap_or(i32::MAX);
    let mut out: Vec<f32> = vec![0.0f32; src.len()];
    // SAFETY: `src` is a readable buffer of `len` shorts; `out` is a writable buffer of `len`
    // floats with the same length. Both pointers are valid for the call.
    unsafe {
        sys::switch_short_to_float(
            src.as_ptr() as *mut i16 as *mut std::os::raw::c_short,
            out.as_mut_ptr(),
            len,
        );
    }
    out
}

/// Converts an array of `f32` (float) samples into `i16` (short) samples.
///
/// FreeSWITCH scales each sample from `[-1.0, 1.0]` back into the signed-16 range, clamping
/// out-of-range values. The returned `Vec` has the same length as `src`.
pub fn float_to_short(src: &[f32]) -> Vec<i16> {
    if src.is_empty() {
        return Vec::new();
    }
    let len = src.len();
    // SAFETY: `switch_float_to_short` takes a `switch_size_t` count (usize).
    let mut out: Vec<i16> = vec![0i16; src.len()];
    // SAFETY: `src` is a readable buffer of `len` floats; `out` is a writable buffer of `len`
    // shorts. The `c_short` alias of `i16` makes the cast sound.
    unsafe {
        sys::switch_float_to_short(
            src.as_ptr() as *mut f32,
            out.as_mut_ptr() as *mut std::os::raw::c_short,
            len,
        );
    }
    out
}

/// Converts an array of `u8` (char) samples into `f32` (float) samples.
///
/// FreeSWITCH treats each byte as a `u8` PCM sample and scales it into the float range. The
/// returned `Vec` has the same length as `src`.
pub fn char_to_float(src: &[u8]) -> Vec<f32> {
    if src.is_empty() {
        return Vec::new();
    }
    let len = i32::try_from(src.len()).unwrap_or(i32::MAX);
    let mut out: Vec<f32> = vec![0.0f32; src.len()];
    // SAFETY: `src` is a readable buffer of `len` chars; `out` is a writable buffer of `len`
    // floats with the same length.
    unsafe {
        sys::switch_char_to_float(
            src.as_ptr() as *mut u8 as *mut std::os::raw::c_char,
            out.as_mut_ptr(),
            len,
        );
    }
    out
}

/// Converts an array of `f32` (float) samples into `u8` (char) samples.
///
/// The returned `Vec` has the same length as `src`.
pub fn float_to_char(src: &[f32]) -> Vec<u8> {
    if src.is_empty() {
        return Vec::new();
    }
    let len = i32::try_from(src.len()).unwrap_or(i32::MAX);
    let mut out: Vec<u8> = vec![0u8; src.len()];
    // SAFETY: `src` is a readable buffer of `len` floats; `out` is a writable buffer of `len`
    // chars with the same length.
    unsafe {
        sys::switch_float_to_char(
            src.as_ptr() as *mut f32,
            out.as_mut_ptr() as *mut std::os::raw::c_char,
            len,
        );
    }
    out
}

/// Byte-swaps a buffer of 16-bit samples in place (endian flip).
pub fn swap_linear(buf: &mut [i16]) {
    if buf.is_empty() {
        return;
    }
    let len = i32::try_from(buf.len()).unwrap_or(i32::MAX);
    // SAFETY: `buf` is a writable buffer of `len` samples.
    unsafe { sys::switch_swap_linear(buf.as_mut_ptr(), len) };
}

/// Fills `data` with static silence, scaled by `divisor` (higher divisor = quieter).
///
/// `samples` is the count of 2-byte samples and `channels` the interleaved channel count.
pub fn generate_sln_silence(data: &mut [i16], channels: u32, divisor: u32) {
    if data.is_empty() {
        return;
    }
    let samples = u32::try_from(data.len()).unwrap_or(u32::MAX);
    // SAFETY: `data` is a writable buffer of `samples` samples.
    unsafe { sys::switch_generate_sln_silence(data.as_mut_ptr(), samples, channels, divisor) };
}

/// Adjusts the volume of a signed-linear buffer in place. `vol` ranges `-4..=4`.
pub fn change_sln_volume(data: &mut [i16], vol: i32) {
    if data.is_empty() {
        return;
    }
    let samples = u32::try_from(data.len()).unwrap_or(u32::MAX);
    // SAFETY: `data` is a writable buffer of `samples` samples.
    unsafe { sys::switch_change_sln_volume(data.as_mut_ptr(), samples, vol) };
}

/// Adjusts the volume of a signed-linear buffer in place with finer granularity. `vol` ranges
/// `-12..=12`.
pub fn change_sln_volume_granular(data: &mut [i16], vol: i32) {
    if data.is_empty() {
        return;
    }
    let samples = u32::try_from(data.len()).unwrap_or(u32::MAX);
    // SAFETY: `data` is a writable buffer of `samples` samples.
    unsafe { sys::switch_change_sln_volume_granular(data.as_mut_ptr(), samples, vol) };
}

/// Mixes `other` into `data` (both signed-linear), returning the resulting sample count.
///
/// `channels` is the interleaved channel count. `data` is modified in place.
pub fn merge_sln(data: &mut [i16], other: &[i16], channels: u32) -> u32 {
    if data.is_empty() {
        return 0;
    }
    let samples = u32::try_from(data.len()).unwrap_or(u32::MAX);
    let other_samples = u32::try_from(other.len()).unwrap_or(u32::MAX);
    let channels_i = i32::try_from(channels).unwrap_or(i32::MAX);
    // SAFETY: `data` is a writable buffer of `samples` samples; `other` is a readable buffer of
    // `other_samples` samples. `channels_i` matches the interleaving of both buffers.
    unsafe {
        sys::switch_merge_sln(
            data.as_mut_ptr(),
            samples,
            other.as_ptr() as *mut i16,
            other_samples,
            channels_i,
        )
    }
}

/// Reverses a prior [`merge_sln`]: subtracts `other` from `data` in place, returning the
/// resulting sample count.
pub fn unmerge_sln(data: &mut [i16], other: &[i16], channels: u32) -> u32 {
    if data.is_empty() {
        return 0;
    }
    let samples = u32::try_from(data.len()).unwrap_or(u32::MAX);
    let other_samples = u32::try_from(other.len()).unwrap_or(u32::MAX);
    let channels_i = i32::try_from(channels).unwrap_or(i32::MAX);
    // SAFETY: `data` is a writable buffer of `samples` samples; `other` is a readable buffer of
    // `other_samples` samples.
    unsafe {
        sys::switch_unmerge_sln(
            data.as_mut_ptr(),
            samples,
            other.as_ptr() as *mut i16,
            other_samples,
            channels_i,
        )
    }
}

/// Mixes `channels` down to (or up to) `orig_channels` in place over `samples` samples.
pub fn mux_channels(data: &mut [i16], orig_channels: u32, channels: u32) {
    if data.is_empty() {
        return;
    }
    let samples = data.len();
    // SAFETY: `data` is a writable buffer of `samples` samples.
    unsafe { sys::switch_mux_channels(data.as_mut_ptr(), samples, orig_channels, channels) };
}

/// Validation tests for [`StreamingResample::new`] input checks.
///
/// Although the `from_rate` / `to_rate` / `quality` checks are pure Rust (they run before the
/// speex handle is built), `StreamingResample` owns a `Resample`, whose `Drop` and `new` pull
/// in `switch_resample_*` symbols. So the whole test binary needs `live_fs` to link — same gate
/// as the rest of `tests`. The validation tests still run in milliseconds once linked.
#[cfg(all(test, feature = "live_fs"))]
mod streaming_validation_tests {
    use super::*;

    #[test]
    fn streaming_rejects_zero_from_rate() {
        assert!(StreamingResample::new(0, 16_000, 3).is_err());
    }

    #[test]
    fn streaming_rejects_zero_to_rate() {
        assert!(StreamingResample::new(8_000, 0, 3).is_err());
    }

    #[test]
    fn streaming_rejects_quality_below_range() {
        assert!(StreamingResample::new(8_000, 16_000, -1).is_err());
    }

    #[test]
    fn streaming_rejects_quality_above_range() {
        assert!(StreamingResample::new(8_000, 16_000, 11).is_err());
    }
}

#[cfg(all(test, feature = "live_fs"))]
mod tests {
    use super::*;

    #[test]
    fn calc_buffer_size_upsamples() {
        // 8000 -> 16000 doubles the sample count.
        assert_eq!(calc_buffer_size(16_000, 8_000, 100), 200);
    }

    #[test]
    fn calc_buffer_size_zero_from_is_zero() {
        assert_eq!(calc_buffer_size(16_000, 0, 100), 0);
    }

    #[test]
    fn short_to_float_round_trips_length() {
        let input = [0i16, i16::MAX, i16::MIN, 1];
        let out = short_to_float(&input);
        assert_eq!(out.len(), input.len());
        assert!(out[0] >= -1.0 && out[0] <= 1.0);
        // Zero stays zero.
        assert!(out[0].abs() < 1e-6);
    }

    #[test]
    fn empty_inputs_yield_empty_outputs() {
        assert!(short_to_float(&[]).is_empty());
        assert!(float_to_short(&[]).is_empty());
        assert!(char_to_float(&[]).is_empty());
        assert!(float_to_char(&[]).is_empty());
    }

    #[test]
    fn agc_config_builder_chains() {
        let cfg = AgcConfig::default()
            .energy_avg(1000)
            .low_energy_point(50)
            .margin(200)
            .change_factor(3)
            .period_len(160);
        assert_eq!(cfg.energy_avg, 1000);
        assert_eq!(cfg.low_energy_point, 50);
        assert_eq!(cfg.margin, 200);
        assert_eq!(cfg.change_factor, 3);
        assert_eq!(cfg.period_len, 160);
    }

    // ── StreamingResample (requires live FreeSWITCH — the constructor builds a real speex
    //     handle, even when the integer-ratio fast path bypasses it at process time). ──────

    #[test]
    fn streaming_create_8k_to_16k() {
        let r = StreamingResample::new(8_000, 16_000, 3).unwrap();
        assert_eq!(r.from_rate(), 8_000);
        assert_eq!(r.to_rate(), 16_000);
        assert!(r.is_integer_upsample());
    }

    #[test]
    fn streaming_create_16k_to_8k_is_not_integer_upsample() {
        // Downsampling never takes the fast path (needs anti-aliasing).
        let r = StreamingResample::new(16_000, 8_000, 3).unwrap();
        assert!(!r.is_integer_upsample());
    }

    #[test]
    fn streaming_process_empty_returns_empty() {
        let mut r = StreamingResample::new(8_000, 16_000, 3).unwrap();
        let mut out = Vec::new();
        r.process_into(&[], &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn streaming_upsample_doubles_length() {
        let mut r = StreamingResample::new(8_000, 16_000, 3).unwrap();
        // 8 kHz sine-ish input; the integer-ratio fast path produces ~2x the samples.
        let input: Vec<i16> = (0..160)
            .map(|i| {
                let t = i as f64 / 8_000.0;
                (10_000.0 * (2.0 * std::f64::consts::PI * 440.0 * t).sin()) as i16
            })
            .collect();
        let out = r.process(&input);
        // The fast path emits exactly `n` samples per input sample, so length is exact.
        assert_eq!(out.len(), input.len() * 2);
    }
}
