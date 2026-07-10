//! Safe, owned wrapper over WebRTC `EchoCanceller3`.
//!
//! [`EchoCanceller3`] is an RAII handle: [`EchoCanceller3::new`] allocates the underlying C++
//! object (default AEC3 config, neural residual echo estimator disabled) and [`Drop`] calls the
//! matching destroy. The handle is neither [`Send`] nor [`Sync`] — AEC3's capture-side methods
//! (`AnalyzeCapture`/`ProcessCapture`) must be serialized, and they mutate C++ state through
//! `&self`-shaped accessors in the WebRTC API.
//!
//! Frames are interleaved signed 16-bit PCM (FreeSWITCH `SLIN16`), one 10 ms frame per call
//! (`sample_rate_hz / 100` samples per channel). The Rust side validates the slice length and
//! channel count before crossing the FFI boundary, since the C ABI only receives a raw pointer
//! and cannot guard a short buffer.

use std::marker::PhantomData;
use std::ptr::NonNull;

use crate::sys;

/// Errors returned by [`EchoCanceller3`] processing.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Aec3Error {
    /// A pointer argument was NULL, the configuration was invalid, or a frame slice was too short.
    InvalidArg,
    /// The channel count didn't match the value passed at [`EchoCanceller3::new`].
    ChannelMismatch,
    /// The frame length isn't exactly `rate/100 * num_channels` samples.
    InvalidFrameLength,
    /// A C++ exception was caught inside the FFI boundary.
    Exception,
    /// Handle allocation / construction failed (e.g. unsupported sample rate).
    CreateFailed,
    /// An unrecognized non-zero status code returned by the C ABI.
    Unknown(i32),
}

impl std::fmt::Display for Aec3Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidArg => write!(f, "invalid argument (null/short/unsupported)"),
            Self::ChannelMismatch => write!(f, "channel count does not match creation"),
            Self::InvalidFrameLength => write!(f, "frame length is not rate/100 * num_channels"),
            Self::Exception => write!(f, "C++ exception inside the FFI boundary"),
            Self::CreateFailed => write!(f, "EchoCanceller3 allocation failed"),
            Self::Unknown(code) => write!(f, "unknown AEC3 status code {code}"),
        }
    }
}

impl std::error::Error for Aec3Error {}

pub type Result<T> = std::result::Result<T, Aec3Error>;

/// Maps a non-zero C ABI status to an [`Aec3Error`]; `0` -> `Ok`.
fn check(status: i32) -> Result<()> {
    match status {
        0 => Ok(()),
        1 => Err(Aec3Error::InvalidArg),
        2 => Err(Aec3Error::ChannelMismatch),
        -1 => Err(Aec3Error::Exception),
        other => Err(Aec3Error::Unknown(other)),
    }
}

/// Echo-return-loss metrics reported by the canceller.
#[derive(Debug, Copy, Clone, Default, PartialEq)]
pub struct Metrics {
    /// Echo return loss (dB) of the echo path.
    pub echo_return_loss: f64,
    /// Echo return loss enhancement (dB) achieved by the canceller.
    pub echo_return_loss_enhancement: f64,
    /// Estimated render-to-capture delay (ms), or 0 if none estimated.
    pub delay_ms: i32,
}

/// An owned WebRTC `EchoCanceller3` handle.
///
/// Constructed with [`EchoCanceller3::new`]; destroyed on [`Drop`] via the C ABI. Feed the
/// far-end (loudspeaker) signal with [`analyze_render`](Self::analyze_render) and remove echo
/// from the near-end (microphone) signal in place with
/// [`process_capture`](Self::process_capture). `AnalyzeRender` is the only method the WebRTC API
/// documents as concurrency-safe with the capture side; all capture-side calls must be
/// serialized by the caller, so this handle is `!Send` / `!Sync`.
pub struct EchoCanceller3 {
    raw: NonNull<sys::fswtch_aec3_t>,
    sample_rate_hz: i32,
    num_render_channels: usize,
    num_capture_channels: usize,
    // `EchoCanceller3` C++ state is mutated through `&self`-shaped accessors and is not
    // thread-safe; capture-side calls must be serialized.
    _marker: PhantomData<*const ()>,
}

impl EchoCanceller3 {
    /// Creates a canceller with the default AEC3 config (neural estimator disabled).
    ///
    /// `sample_rate_hz` must be an AEC3-supported rate (8000/16000/32000/48000). The 16 kHz /
    /// 1-band path is the recommended default (no band splitting, so the QMF/resampler stubs are
    /// never exercised); 48 kHz uses the real `three_band_filter_bank`. 32 kHz (2-band QMF) is
    /// not recommended until the QMF shim is replaced.
    pub fn new(
        sample_rate_hz: i32,
        num_render_channels: usize,
        num_capture_channels: usize,
    ) -> Result<Self> {
        if sample_rate_hz <= 0 || num_render_channels == 0 || num_capture_channels == 0 {
            return Err(Aec3Error::InvalidArg);
        }
        // SAFETY: `fswtch_aec3_create` performs no I/O and takes only by-value primitives; a null
        // return signals allocation/construction failure (mapped to CreateFailed).
        let raw = unsafe {
            sys::fswtch_aec3_create(sample_rate_hz, num_render_channels, num_capture_channels)
        };
        let raw = NonNull::new(raw).ok_or(Aec3Error::CreateFailed)?;
        Ok(Self {
            raw,
            sample_rate_hz,
            num_render_channels,
            num_capture_channels,
            _marker: PhantomData,
        })
    }

    /// Number of interleaved samples in one 10 ms frame for `num_channels` channels.
    fn frame_len(&self, num_channels: usize) -> usize {
        (self.sample_rate_hz as usize / 100) * num_channels
    }

    /// Feeds one 10 ms far-end (loudspeaker) render frame to the canceller.
    ///
    /// `frame` must contain exactly `rate/100 * num_render_channels` interleaved samples;
    /// `num_channels` must equal the render channel count passed to [`new`](Self::new).
    pub fn analyze_render(&mut self, frame: &[i16], num_channels: usize) -> Result<()> {
        if num_channels != self.num_render_channels {
            return Err(Aec3Error::ChannelMismatch);
        }
        if frame.len() != self.frame_len(num_channels) {
            return Err(Aec3Error::InvalidFrameLength);
        }
        // SAFETY: `self.raw` is a live owned handle. `frame.as_ptr()`/`len()` describe a valid
        // `int16_t` buffer of exactly `frame_len` samples for the duration of the call (validated
        // above); AEC3 reads `rate/100 * num_channels` samples from it.
        let status = unsafe {
            sys::fswtch_aec3_analyze_render(self.raw.as_ptr(), frame.as_ptr(), num_channels)
        };
        check(status)
    }

    /// Removes echo from one 10 ms near-end (microphone) capture frame, in place.
    ///
    /// Analyzes saturation then processes the capture signal, writing the cleaned samples back
    /// into `frame`. `frame` must contain exactly `rate/100 * num_capture_channels` interleaved
    /// samples; `num_channels` must equal the capture channel count passed to
    /// [`new`](Self::new). Set `level_change` when the capture gain is known to have changed
    /// since the last frame (toggles AEC3's filter-divergence protection).
    pub fn process_capture(
        &mut self,
        frame: &mut [i16],
        num_channels: usize,
        level_change: bool,
    ) -> Result<()> {
        if num_channels != self.num_capture_channels {
            return Err(Aec3Error::ChannelMismatch);
        }
        if frame.len() != self.frame_len(num_channels) {
            return Err(Aec3Error::InvalidFrameLength);
        }
        // SAFETY: `self.raw` is a live owned handle. `frame.as_mut_ptr()`/`len()` describe a valid
        // mutable `int16_t` buffer of exactly `frame_len` samples; AEC3 reads and writes back at
        // most that many samples. Capture-side calls are serialized (this method takes `&mut self`).
        let status = unsafe {
            sys::fswtch_aec3_process_capture(
                self.raw.as_ptr(),
                frame.as_mut_ptr(),
                num_channels,
                level_change as i32,
            )
        };
        check(status)
    }

    /// Sets an external estimate of the render-to-capture audio buffer delay (ms).
    pub fn set_delay(&mut self, delay_ms: i32) {
        // SAFETY: `self.raw` is live; `SetAudioBufferDelay` takes an int and stores it.
        unsafe { sys::fswtch_aec3_set_audio_buffer_delay(self.raw.as_ptr(), delay_ms) };
    }

    /// Whether the canceller is actively processing.
    pub fn active_processing(&self) -> bool {
        // SAFETY: `self.raw` is live; `ActiveProcessing` is a const read-only call.
        let active = unsafe {
            sys::fswtch_aec3_active_processing(self.raw.as_ptr() as *const sys::fswtch_aec3_t)
        };
        active != 0
    }

    /// Current echo-return-loss metrics.
    pub fn get_metrics(&self) -> Metrics {
        let mut erl = 0.0_f64;
        let mut erle = 0.0_f64;
        let mut delay_ms = 0_i32;
        // SAFETY: `self.raw` is live; `GetMetrics` writes into the three out-pointers, which are
        // valid `f64`/`i32` locals for the duration of the call.
        unsafe {
            sys::fswtch_aec3_get_metrics(
                self.raw.as_ptr() as *const sys::fswtch_aec3_t,
                &mut erl,
                &mut erle,
                &mut delay_ms,
            );
        }
        Metrics {
            echo_return_loss: erl,
            echo_return_loss_enhancement: erle,
            delay_ms,
        }
    }

    /// The raw `fswtch_aec3_t` pointer for escape-hatch FFI.
    #[inline]
    pub fn as_ptr(&self) -> *mut sys::fswtch_aec3_t {
        self.raw.as_ptr()
    }

    /// Wraps an existing raw handle created elsewhere.
    ///
    /// # Safety
    ///
    /// `raw` must point to a live `fswtch_aec3_t` that the caller is willing to hand over for
    /// destruction via `fswtch_aec3_destroy` when this [`EchoCanceller3`] is dropped. The
    /// `sample_rate_hz` / channel counts must match those the handle was created with, since
    /// they drive frame-length validation. Wrapping a handle already owned by another
    /// `EchoCanceller3` (or any RAII guard) is unsound — it would be freed twice.
    pub unsafe fn from_raw(
        raw: *mut sys::fswtch_aec3_t,
        sample_rate_hz: i32,
        num_render_channels: usize,
        num_capture_channels: usize,
    ) -> Option<Self> {
        NonNull::new(raw).map(|raw| Self {
            raw,
            sample_rate_hz,
            num_render_channels,
            num_capture_channels,
            _marker: PhantomData,
        })
    }
}

impl Drop for EchoCanceller3 {
    fn drop(&mut self) {
        // SAFETY: `self.raw` owns exactly one `fswtch_aec3_t` allocated by `fswtch_aec3_create`
        // in `new`. `fswtch_aec3_destroy` releases the C++ allocation; the handle is not shared
        // — this `EchoCanceller3` owns it exclusively — so a single destroy is correct.
        unsafe { sys::fswtch_aec3_destroy(self.raw.as_ptr()) };
    }
}

impl std::fmt::Debug for EchoCanceller3 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EchoCanceller3")
            .field("ptr", &self.raw)
            .field("rate_hz", &self.sample_rate_hz)
            .field("render_ch", &self.num_render_channels)
            .field("capture_ch", &self.num_capture_channels)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: i32 = 16_000;
    const CH: usize = 1;
    const FRAME: usize = (RATE / 100) as usize * CH; // 160 samples / 10 ms

    #[test]
    fn new_constructs_and_drops_cleanly() {
        let aec = EchoCanceller3::new(RATE, CH, CH);
        assert!(aec.is_ok());
        // Drop runs here; under ASan/LSan this would surface a leak or double-free.
        drop(aec.unwrap());
    }

    #[test]
    fn rejects_bad_config() {
        assert_eq!(
            EchoCanceller3::new(0, CH, CH).unwrap_err(),
            Aec3Error::InvalidArg
        );
        assert_eq!(
            EchoCanceller3::new(RATE, 0, CH).unwrap_err(),
            Aec3Error::InvalidArg
        );
    }

    #[test]
    fn process_pipeline_runs_on_real_aec3() {
        let mut aec = EchoCanceller3::new(RATE, CH, CH).expect("create");
        let render = vec![0i16; FRAME];
        let mut capture = vec![0i16; FRAME];
        for _ in 0..20 {
            aec.analyze_render(&render, CH).expect("analyze_render");
            aec.process_capture(&mut capture, CH, false)
                .expect("process_capture");
        }
        // active_processing + metrics are observable without crashing; exact values depend on
        // the (zero) signal and aren't asserted here — equivalence is Phase 5.
        let _active = aec.active_processing();
        let _metrics = aec.get_metrics();
    }

    #[test]
    fn channel_mismatch_is_rejected_before_ffi() {
        let mut aec = EchoCanceller3::new(RATE, CH, CH).expect("create");
        let render = vec![0i16; FRAME];
        // Wrong channel count (2 vs creation's 1) — must be caught in Rust, never reach the C ABI.
        assert_eq!(
            aec.analyze_render(&render, 2).unwrap_err(),
            Aec3Error::ChannelMismatch
        );
    }

    #[test]
    fn wrong_frame_length_is_rejected() {
        let mut aec = EchoCanceller3::new(RATE, CH, CH).expect("create");
        let short = vec![0i16; FRAME - 1];
        assert_eq!(
            aec.analyze_render(&short, CH).unwrap_err(),
            Aec3Error::InvalidFrameLength
        );
    }

    #[test]
    fn set_delay_does_not_panic() {
        let mut aec = EchoCanceller3::new(RATE, CH, CH).expect("create");
        aec.set_delay(50);
    }

    /// Functional echo-cancellation check (Phase 5). Feeds deterministic broadband noise as the
    /// render (far-end) and a delayed copy of it as the capture (pure echo, no near-end). A
    /// correctly-wired wrapper plus a working AEC3 must suppress the echo: post-convergence
    /// output energy should be well below input energy (high ERLE). If the framing/buffering were
    /// wrong, the adaptive filter could never align render with capture and ERLE would stay near
    /// 0 dB. (>6 dB is lenient; AEC3 on pure broadband echo typically reaches >15 dB.)
    #[test]
    fn cancels_a_real_echo() {
        let mut aec = EchoCanceller3::new(RATE, CH, CH).expect("create");
        const N_FRAMES: usize = 300; // 3 s
        const WARMUP: usize = 150; // 1.5 s for filter convergence
        const ECHO_DELAY: usize = 64; // 4 ms, within AEC3's delay search range
        // Deterministic LCG so the test is reproducible.
        let mut lcg = 1u32;
        let mut render = vec![0i16; N_FRAMES * FRAME];
        for s in render.iter_mut() {
            lcg = lcg.wrapping_mul(1664525).wrapping_add(1013904223);
            *s = (((lcg >> 16) as i32 % 8000) - 4000) as i16;
        }
        // capture = render delayed by ECHO_DELAY (pure echo); zero before the delay fills.
        let mut capture = vec![0i16; N_FRAMES * FRAME];
        capture[ECHO_DELAY..].copy_from_slice(&render[..render.len() - ECHO_DELAY]);

        let mut in_energy = 0.0_f64;
        let mut out_energy = 0.0_f64;
        for f in 0..N_FRAMES {
            let r = &render[f * FRAME..(f + 1) * FRAME];
            let c = &mut capture[f * FRAME..(f + 1) * FRAME];
            aec.analyze_render(r, CH).expect("analyze_render");
            if f >= WARMUP {
                for &s in c.iter() {
                    in_energy += (s as f64) * (s as f64);
                }
            }
            aec.process_capture(c, CH, false).expect("process_capture");
            if f >= WARMUP {
                for &s in c.iter() {
                    out_energy += (s as f64) * (s as f64);
                }
            }
        }
        let erle_db = 10.0 * (in_energy / out_energy.max(1e-12)).log10();
        // Observed ~67 dB on aarch64 (pure echo, no near-end → near-full cancellation). >15 dB is
        // a safe-but-meaningful bar: a broken wrapper (out-of-sync render/capture) stays ~0 dB.
        assert!(
            erle_db > 15.0,
            "AEC3 did not suppress the echo: ERLE = {erle_db:.1} dB \
             (in_energy={in_energy:.0}, out_energy={out_energy:.0})"
        );
    }
}
