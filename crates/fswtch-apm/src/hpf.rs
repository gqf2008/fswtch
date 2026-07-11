//! Safe owned wrapper over WebRTC's `HighPassFilter`.
//!
//! [`HighPassFilter`] removes low-frequency / DC content below ~80 Hz from each 10 ms frame.
//! Constructed with [`HighPassFilter::new`]; destroyed on [`Drop`]. Frames are interleaved
//! signed 16-bit PCM, one 10 ms frame per call. Like [`crate::EchoCanceller3`], the handle is
//! `!Send` / `!Sync` (the WebRTC filter mutates state through `&self`-shaped accessors).

use std::marker::PhantomData;
use std::ptr::NonNull;

// The error/result/check helpers are shared with the AEC3 wrapper (the C ABI uses one status
// convention across every module); reuse them under the generic name `Error`.
use crate::aec3::{Aec3Error as Error, Result, check};
use crate::sys;

/// An owned WebRTC `HighPassFilter` handle.
pub struct HighPassFilter {
    raw: NonNull<sys::fswtch_hpf_t>,
    sample_rate_hz: i32,
    num_channels: usize,
    // `HighPassFilter` C++ state is mutated through `&self`-shaped accessors and is not
    // thread-safe.
    _marker: PhantomData<*const ()>,
}

impl HighPassFilter {
    /// Creates a high-pass filter for `sample_rate_hz` / `num_channels`.
    pub fn new(sample_rate_hz: i32, num_channels: usize) -> Result<Self> {
        if !matches!(sample_rate_hz, 8000 | 16000 | 48000) || num_channels == 0 {
            return Err(Error::InvalidArg);
        }
        // SAFETY: `fswtch_hpf_create` performs no I/O and takes only by-value primitives; a null
        // return signals allocation failure (mapped to CreateFailed).
        let raw = unsafe { sys::fswtch_hpf_create(sample_rate_hz, num_channels) };
        let raw = NonNull::new(raw).ok_or(Error::CreateFailed)?;
        Ok(Self {
            raw,
            sample_rate_hz,
            num_channels,
            _marker: PhantomData,
        })
    }

    /// High-pass filters one 10 ms interleaved `i16` frame in place.
    ///
    /// `frame` must contain exactly `rate/100 * num_channels` samples; `num_channels` must
    /// equal the count passed to [`new`](Self::new).
    pub fn process(&mut self, frame: &mut [i16], num_channels: usize) -> Result<()> {
        if num_channels != self.num_channels {
            return Err(Error::ChannelMismatch);
        }
        let expected = (self.sample_rate_hz as usize / 100) * num_channels;
        if frame.len() != expected {
            return Err(Error::InvalidFrameLength);
        }
        // SAFETY: `self.raw` is a live owned handle; `frame.as_mut_ptr()`/`len()` describe a
        // valid mutable `int16_t` buffer of exactly `expected` samples for the call.
        let status =
            unsafe { sys::fswtch_hpf_process(self.raw.as_ptr(), frame.as_mut_ptr(), num_channels) };
        check(status)
    }

    /// The raw `fswtch_hpf_t` pointer for escape-hatch FFI.
    #[inline]
    pub fn as_ptr(&self) -> *mut sys::fswtch_hpf_t {
        self.raw.as_ptr()
    }

    /// Wraps an existing raw handle created elsewhere.
    ///
    /// # Safety
    ///
    /// `raw` must point to a live `fswtch_hpf_t` that the caller is willing to hand over for
    /// destruction via `fswtch_hpf_destroy` when this [`HighPassFilter`] is dropped. The
    /// `sample_rate_hz` / `num_channels` must match those the handle was created with.
    pub unsafe fn from_raw(
        raw: *mut sys::fswtch_hpf_t,
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

impl Drop for HighPassFilter {
    fn drop(&mut self) {
        // SAFETY: `self.raw` owns exactly one `fswtch_hpf_t` allocated by `fswtch_hpf_create`;
        // `fswtch_hpf_destroy` releases it. The handle is exclusively owned.
        unsafe { sys::fswtch_hpf_destroy(self.raw.as_ptr()) };
    }
}

impl std::fmt::Debug for HighPassFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HighPassFilter")
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
    const FRAME: usize = (RATE as usize) / 100 * CH; // 160 samples / 10 ms

    #[test]
    fn new_and_drop_clean() {
        let h = HighPassFilter::new(RATE, CH);
        assert!(h.is_ok());
        drop(h.unwrap());
    }

    #[test]
    fn rejects_bad_args() {
        assert_eq!(HighPassFilter::new(0, CH).unwrap_err(), Error::InvalidArg);
        assert_eq!(HighPassFilter::new(RATE, 0).unwrap_err(), Error::InvalidArg);
    }

    #[test]
    fn attenuates_dc() {
        // A high-pass filter must remove a DC (0 Hz) offset: feed a constant 100 for ~200 ms
        // and assert the output mean drops well below the input level.
        let mut h = HighPassFilter::new(RATE, CH).expect("create");
        let mut frame = vec![100i16; FRAME];
        for _ in 0..20 {
            h.process(&mut frame, CH).expect("process");
        }
        let mean: f64 = frame.iter().map(|&s| s as f64).sum::<f64>() / frame.len() as f64;
        assert!(
            mean.abs() < 40.0,
            "HPF did not attenuate DC: mean = {mean:.2} (input was 100)"
        );
    }

    #[test]
    fn rejects_wrong_frame_length() {
        let mut h = HighPassFilter::new(RATE, CH).expect("create");
        let mut short = vec![0i16; FRAME - 1];
        assert_eq!(
            h.process(&mut short, CH).unwrap_err(),
            Error::InvalidFrameLength
        );
    }
}
