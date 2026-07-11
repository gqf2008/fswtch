//! Safe owned wrapper over WebRTC AGC2 (automatic gain control 2).
//!
//! This is the **scalar** AGC2 path: a fixed digital gain followed by an optional hard limiter.
//! The adaptive digital path (RNN-based loudness → adaptive gain) + the analog input-volume
//! controller are disabled, exactly as `GainController2` does when `adaptive_digital.enabled`
//! and `input_volume_controller.enabled` are both `false`. Constructed with
//! [`GainController2::new`]; destroyed on [`Drop`]. Frames are interleaved signed 16-bit PCM.
//! `!Send` / `!Sync`.

use std::marker::PhantomData;
use std::ptr::NonNull;

use crate::sys;
use crate::{Error, Result, check};

/// An owned WebRTC AGC2 handle (fixed digital gain + optional limiter).
pub struct GainController2 {
    raw: NonNull<sys::fswtch_agc2_t>,
    sample_rate_hz: i32,
    num_channels: usize,
    _marker: PhantomData<*const ()>,
}

impl GainController2 {
    /// Creates an AGC2 with a fixed digital gain (`fixed_gain_db`, e.g. 0..30 dB) and an optional
    /// limiter. The limiter compresses peaks that would otherwise clip after the fixed gain.
    pub fn new(
        fixed_gain_db: f32,
        limiter_enabled: bool,
        sample_rate_hz: i32,
        num_channels: usize,
    ) -> Result<Self> {
        if !matches!(sample_rate_hz, 8000 | 16000 | 48000) || num_channels == 0 {
            return Err(Error::InvalidArg);
        }
        // SAFETY: `fswtch_agc2_create` performs no I/O and takes only by-value primitives; a null
        // return signals allocation failure (mapped to CreateFailed).
        let raw = unsafe {
            sys::fswtch_agc2_create(
                fixed_gain_db,
                limiter_enabled as i32,
                sample_rate_hz,
                num_channels,
            )
        };
        let raw = NonNull::new(raw).ok_or(Error::CreateFailed)?;
        Ok(Self {
            raw,
            sample_rate_hz,
            num_channels,
            _marker: PhantomData,
        })
    }

    /// Applies the fixed gain (+ limiter if enabled) to one 10 ms interleaved `i16` frame in place.
    ///
    /// `frame` must contain exactly `rate/100 * num_channels` samples; `num_channels` must equal
    /// the count passed to [`new`](Self::new).
    pub fn process(&mut self, frame: &mut [i16], num_channels: usize) -> Result<()> {
        if num_channels != self.num_channels {
            return Err(Error::ChannelMismatch);
        }
        let expected = (self.sample_rate_hz as usize / 100) * num_channels;
        if frame.len() != expected {
            return Err(Error::InvalidFrameLength);
        }
        // SAFETY: `self.raw` is a live owned handle; `frame.as_mut_ptr()`/`len()` describe a valid
        // mutable `int16_t` buffer of exactly `expected` samples for the call.
        let status = unsafe {
            sys::fswtch_agc2_process(self.raw.as_ptr(), frame.as_mut_ptr(), num_channels)
        };
        check(status)
    }

    /// The raw `fswtch_agc2_t` pointer for escape-hatch FFI.
    #[inline]
    pub fn as_ptr(&self) -> *mut sys::fswtch_agc2_t {
        self.raw.as_ptr()
    }

    /// Wraps an existing raw handle created elsewhere.
    ///
    /// # Safety
    ///
    /// `raw` must point to a live `fswtch_agc2_t` that the caller is willing to hand over for
    /// destruction via `fswtch_agc2_destroy` when this [`GainController2`] is dropped. The rate /
    /// channel count must match those the handle was created with.
    pub unsafe fn from_raw(
        raw: *mut sys::fswtch_agc2_t,
        sample_rate_hz: i32,
        num_channels: usize,
    ) -> Option<Self> {
        NonNull::new(raw).map(|raw| Self {
            raw,
            sample_rate_hz,
            num_channels,
            _marker: PhantomData,
        })
    }
}

impl Drop for GainController2 {
    fn drop(&mut self) {
        // SAFETY: `self.raw` owns exactly one `fswtch_agc2_t` allocated by `fswtch_agc2_create`;
        // `fswtch_agc2_destroy` releases it. The handle is exclusively owned.
        unsafe { sys::fswtch_agc2_destroy(self.raw.as_ptr()) };
    }
}

impl std::fmt::Debug for GainController2 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GainController2")
            .field("ptr", &self.raw)
            .field("rate_hz", &self.sample_rate_hz)
            .field("channels", &self.num_channels)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: i32 = 16_000;
    const CH: usize = 1;
    const FRAME: usize = (RATE as usize) / 100 * CH;

    // A 1 kHz sine of the given peak amplitude across one 10 ms frame.
    fn sine(amp: f32) -> Vec<i16> {
        (0..FRAME)
            .map(|i| {
                (amp * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / RATE as f32).sin()) as i16
            })
            .collect()
    }
    fn rms(frame: &[i16]) -> f64 {
        let sum: f64 = frame.iter().map(|&s| (s as f64).powi(2)).sum();
        (sum / frame.len() as f64).sqrt()
    }

    #[test]
    fn new_and_drop_clean() {
        let g = GainController2::new(6.0, true, RATE, CH);
        assert!(g.is_ok());
        drop(g.unwrap());
    }

    #[test]
    fn rejects_bad_args() {
        assert_eq!(
            GainController2::new(6.0, true, 0, CH).unwrap_err(),
            Error::InvalidArg
        );
        assert_eq!(
            GainController2::new(6.0, true, RATE, 0).unwrap_err(),
            Error::InvalidArg
        );
    }

    #[test]
    fn applies_fixed_gain() {
        // +6 dB ≈ ×2.0 gain, limiter OFF (amp is small enough to never clip): output RMS ≈ 2× input.
        let mut g = GainController2::new(6.0, false, RATE, CH).expect("create");
        let input = sine(1000.0);
        let in_rms = rms(&input);
        // One process call applies the fixed gain once (calling it repeatedly on the same buffer
        // would compound the gain — GainApplier multiplies in place each call).
        let mut frame = input.clone();
        g.process(&mut frame, CH).expect("process");
        let out_rms = rms(&frame);
        let ratio = out_rms / in_rms;
        assert!(
            (1.8..=2.2).contains(&ratio),
            "fixed gain not ~×2 for +6 dB: ratio = {ratio:.3} (in_rms={in_rms:.1}, out_rms={out_rms:.1})"
        );
    }

    #[test]
    fn limiter_engages() {
        // +6 dB (×2) on amp 25000 -> ~50000 (beyond i16 range). Without a limiter, CopyTo hard-clips
        // the peaks flat (raising RMS); with the limiter ON, the signal is gain-reduced toward
        // 0 dBFS first, yielding a cleaner sine with *lower* RMS. Both clip to |32768| at the
        // int16 boundary, so RMS — not peak — distinguishes engagement.
        let mut on = GainController2::new(6.0, true, RATE, CH).expect("create");
        let mut off = GainController2::new(6.0, false, RATE, CH).expect("create");
        let mut f_on = sine(25000.0);
        let mut f_off = sine(25000.0);
        for _ in 0..20 {
            // Fresh frame each call so the fixed gain doesn't compound.
            f_on = sine(25000.0);
            f_off = sine(25000.0);
            on.process(&mut f_on, CH).expect("on");
            off.process(&mut f_off, CH).expect("off");
        }
        let on_rms = rms(&f_on);
        let off_rms = rms(&f_off);
        assert!(
            on_rms < off_rms,
            "limiter did not reduce RMS vs hard-clip: on={on_rms:.1}, off={off_rms:.1}"
        );
    }

    #[test]
    fn rejects_wrong_frame_length() {
        let mut g = GainController2::new(6.0, true, RATE, CH).expect("create");
        let mut short = vec![0i16; FRAME - 1];
        assert_eq!(
            g.process(&mut short, CH).unwrap_err(),
            Error::InvalidFrameLength
        );
    }
}
