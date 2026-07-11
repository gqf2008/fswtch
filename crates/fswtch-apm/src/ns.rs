//! Safe owned wrapper over WebRTC's `NoiseSuppressor` (spectral noise suppression).
//!
//! [`NoiseSuppressor`] estimates a stationary noise floor and attenuates it per 10 ms frame.
//! Constructed with [`NoiseSuppressor::new`]; destroyed on [`Drop`]. Frames are interleaved
//! signed 16-bit PCM. Like the other modules, the handle is `!Send` / `!Sync`.

use std::marker::PhantomData;
use std::ptr::NonNull;

use crate::aec3::{Aec3Error as Error, Result, check};
use crate::sys;

/// Noise suppression level (dB of attenuation).
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum NsLevel {
    /// 6 dB.
    Db6,
    /// 12 dB (the WebRTC default).
    Db12,
    /// 18 dB.
    Db18,
    /// 21 dB.
    Db21,
}

impl NsLevel {
    fn as_i32(self) -> i32 {
        match self {
            Self::Db6 => 0,
            Self::Db12 => 1,
            Self::Db18 => 2,
            Self::Db21 => 3,
        }
    }
}

/// An owned WebRTC `NoiseSuppressor` handle.
pub struct NoiseSuppressor {
    raw: NonNull<sys::fswtch_ns_t>,
    sample_rate_hz: i32,
    num_channels: usize,
    _marker: PhantomData<*const ()>,
}

impl NoiseSuppressor {
    /// Creates a noise suppressor with `level` for `sample_rate_hz` / `num_channels`.
    pub fn new(level: NsLevel, sample_rate_hz: i32, num_channels: usize) -> Result<Self> {
        if !matches!(sample_rate_hz, 8000 | 16000 | 48000) || num_channels == 0 {
            return Err(Error::InvalidArg);
        }
        // SAFETY: `fswtch_ns_create` performs no I/O and takes only by-value primitives; a null
        // return signals allocation failure (mapped to CreateFailed).
        let raw = unsafe { sys::fswtch_ns_create(level.as_i32(), sample_rate_hz, num_channels) };
        let raw = NonNull::new(raw).ok_or(Error::CreateFailed)?;
        Ok(Self {
            raw,
            sample_rate_hz,
            num_channels,
            _marker: PhantomData,
        })
    }

    /// Suppresses noise in one 10 ms interleaved `i16` frame in place (analyzes then processes).
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
        let status =
            unsafe { sys::fswtch_ns_process(self.raw.as_ptr(), frame.as_mut_ptr(), num_channels) };
        check(status)
    }

    /// The raw `fswtch_ns_t` pointer for escape-hatch FFI.
    #[inline]
    pub fn as_ptr(&self) -> *mut sys::fswtch_ns_t {
        self.raw.as_ptr()
    }

    /// Wraps an existing raw handle created elsewhere.
    ///
    /// # Safety
    ///
    /// `raw` must point to a live `fswtch_ns_t` that the caller is willing to hand over for
    /// destruction via `fswtch_ns_destroy` when this [`NoiseSuppressor`] is dropped. The rate /
    /// channel count must match those the handle was created with.
    pub unsafe fn from_raw(
        raw: *mut sys::fswtch_ns_t,
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

impl Drop for NoiseSuppressor {
    fn drop(&mut self) {
        // SAFETY: `self.raw` owns exactly one `fswtch_ns_t` allocated by `fswtch_ns_create`;
        // `fswtch_ns_destroy` releases it. The handle is exclusively owned.
        unsafe { sys::fswtch_ns_destroy(self.raw.as_ptr()) };
    }
}

impl std::fmt::Debug for NoiseSuppressor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NoiseSuppressor")
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

    #[test]
    fn new_and_drop_clean() {
        let ns = NoiseSuppressor::new(NsLevel::Db12, RATE, CH);
        assert!(ns.is_ok());
        drop(ns.unwrap());
    }

    #[test]
    fn rejects_bad_args() {
        assert_eq!(
            NoiseSuppressor::new(NsLevel::Db12, 0, CH).unwrap_err(),
            Error::InvalidArg
        );
        assert_eq!(
            NoiseSuppressor::new(NsLevel::Db12, RATE, 0).unwrap_err(),
            Error::InvalidArg
        );
    }

    #[test]
    fn suppresses_broadband_noise() {
        // NS must attenuate stationary broadband noise: feed deterministic LCG noise for ~1 s
        // (warmup) then measure input vs output energy. Output should be well below input.
        let mut ns = NoiseSuppressor::new(NsLevel::Db12, RATE, CH).expect("create");
        const N: usize = 100;
        const WARMUP: usize = 50;
        let mut lcg = 1u32;
        let mut frame = vec![0i16; FRAME];
        let mut in_energy = 0.0_f64;
        let mut out_energy = 0.0_f64;
        for f in 0..N {
            for s in frame.iter_mut() {
                lcg = lcg.wrapping_mul(1664525).wrapping_add(1013904223);
                *s = (((lcg >> 16) as i32 % 8000) - 4000) as i16;
            }
            if f >= WARMUP {
                for &s in frame.iter() {
                    in_energy += (s as f64) * (s as f64);
                }
            }
            ns.process(&mut frame, CH).expect("process");
            if f >= WARMUP {
                for &s in frame.iter() {
                    out_energy += (s as f64) * (s as f64);
                }
            }
        }
        assert!(
            out_energy < 0.1 * in_energy,
            "NS did not suppress noise: out/in = {:.3} (in={in_energy:.0}, out={out_energy:.0})",
            out_energy / in_energy
        );
    }

    #[test]
    fn rejects_wrong_frame_length() {
        let mut ns = NoiseSuppressor::new(NsLevel::Db12, RATE, CH).expect("create");
        let mut short = vec![0i16; FRAME - 1];
        assert_eq!(
            ns.process(&mut short, CH).unwrap_err(),
            Error::InvalidFrameLength
        );
    }
}
