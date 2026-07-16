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
}
