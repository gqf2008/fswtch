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
    // The C struct is opaque to bindgen, so we mirror the rate here for frame math in
    // `speech_segments`. `sample_rate` is the *effective* rate (0 → 8000, matching `switch_vad_init`).
    sample_rate: u32,
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
        // `switch_vad_init` substitutes 8000 for a zero/negative rate; mirror that so the frame
        // math in `speech_segments` agrees with the rate the detector actually runs at.
        let sample_rate = if sample_rate > 0 {
            sample_rate as u32
        } else {
            8000
        };
        NonNull::new(raw)
            .map(|raw| Self {
                raw,
                sample_rate,
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

    /// The sample rate this VAD runs at (the effective rate — `0` passed to [`new`](Self::new)
    /// is reported as `8000`, matching `switch_vad_init`).
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Feeds `pcm` through the VAD in `frame_ms`-millisecond frames and returns the detected
    /// speech segments, **snapped to the real energy envelope**.
    ///
    /// The underlying FreeSWITCH VAD uses hysteresis (`voice_ms`=200 to start, `silence_ms`=500
    /// to stop), which truncates each utterance's onset and pads a ~500 ms silent tail — fine
    /// for a call state machine, but loose for slicing audio to feed an LLM/ASR. This method
    /// runs that coarse detector, then [`snap_segments`] each result: start moved back to the
    /// speech onset, trailing silence trimmed, using a per-segment floor of peak − 30 dB.
    ///
    /// `frame_ms` must be a value the detector accepts (10/20/30 when fvad is enabled via
    /// [`set_mode`](Self::set_mode); any when on the native energy path). Segment bounds are in
    /// **samples**; use [`SpeechSegment::duration_ms`] / [`SpeechSegment::samples`].
    pub fn speech_segments(&self, pcm: &[i16], frame_ms: u32) -> Vec<SpeechSegment> {
        let frame = self.frame_samples(frame_ms);
        let mut segs = self.coarse_segments(pcm, frame);
        snap_segments(pcm, frame, &mut segs);
        segs
    }

    /// The raw hysteresis segments (before snapping): `START_TALKING`..`STOP_TALKING`
    /// transitions driven by [`process`](Self::process). Exposed so callers can apply their own
    /// post-processing; prefer [`speech_segments`](Self::speech_segments) for LLM/ASR slicing.
    fn coarse_segments(&self, pcm: &[i16], frame: usize) -> Vec<SpeechSegment> {
        if frame == 0 || pcm.is_empty() {
            return Vec::new();
        }
        let mut segs = Vec::new();
        let mut scratch = vec![0i16; frame];
        let mut in_speech = false;
        let mut seg_start = 0usize;
        let mut off = 0usize;
        while off < pcm.len() {
            let n = frame.min(pcm.len() - off);
            scratch[..n].copy_from_slice(&pcm[off..off + n]);
            if n < frame {
                scratch[n..].fill(0);
            }
            let st = self.process(&mut scratch);
            let end = (off + frame).min(pcm.len());
            if st == VadState::START_TALKING && !in_speech {
                seg_start = off;
                in_speech = true;
            } else if st == VadState::STOP_TALKING && in_speech {
                segs.push(SpeechSegment {
                    start_sample: seg_start,
                    end_sample: end,
                });
                in_speech = false;
            }
            off += frame;
        }
        if in_speech {
            segs.push(SpeechSegment {
                start_sample: seg_start,
                end_sample: pcm.len(),
            });
        }
        segs
    }

    fn frame_samples(&self, frame_ms: u32) -> usize {
        (self.sample_rate as u64 * frame_ms as u64 / 1000) as usize
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

/// A contiguous run of speech detected by [`Vad::speech_segments`], with bounds snapped to the
/// real energy envelope (not the raw hysteresis transitions).
///
/// `start_sample` is inclusive, `end_sample` is exclusive (half-open, like a slice).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpeechSegment {
    /// First sample index of the segment.
    pub start_sample: usize,
    /// One-past-the-last sample index of the segment.
    pub end_sample: usize,
}

impl SpeechSegment {
    /// Segment duration in milliseconds at `sample_rate` Hz.
    pub fn duration_ms(&self, sample_rate: u32) -> u32 {
        ((self.end_sample - self.start_sample) as u32 * 1000) / sample_rate
    }

    /// The slice of `pcm` this segment covers (clamped to `pcm.len()`).
    pub fn samples<'a>(&self, pcm: &'a [i16]) -> &'a [i16] {
        &pcm[self.start_sample..self.end_sample.min(pcm.len())]
    }
}

/// Refines coarse VAD hysteresis segments to the real speech energy envelope.
///
/// For each segment: extends the start backward to the speech onset (recovering what the
/// `voice_ms` hysteresis truncated) and trims the trailing silence (removing the `silence_ms`
/// hangover tail), using a per-segment energy floor of **peak − 30 dB** (linear: `peak / 31.62`,
/// with a ~−90 dBFS minimum). Bounds never move outside the original coarse segment's span.
///
/// Pure Rust (no FFI), so it works on segments from any detector, not just [`Vad`].
pub fn snap_segments(pcm: &[i16], frame: usize, segs: &mut [SpeechSegment]) {
    if frame == 0 || pcm.is_empty() {
        return;
    }
    let nfr = pcm.len() / frame + usize::from(!pcm.len().is_multiple_of(frame));
    // Per-frame RMS (linear 0..32768). A single frame's squared-sum never overflows i64.
    let rms: Vec<f64> = (0..nfr)
        .map(|i| {
            let off = i * frame;
            let end = (off + frame).min(pcm.len());
            let n = end - off;
            if n == 0 {
                0.0
            } else {
                let sq: u64 = pcm[off..end]
                    .iter()
                    .map(|&s| (s as i64 * s as i64) as u64)
                    .sum();
                (sq as f64 / n as f64).sqrt()
            }
        })
        .collect();
    for seg in segs.iter_mut() {
        let s = (seg.start_sample / frame).min(nfr);
        let e = (seg.end_sample / frame).min(nfr);
        if e <= s {
            continue;
        }
        let peak = rms[s..e].iter().fold(0.0f64, |a, &b| a.max(b));
        let floor = (peak / 31.62).max(1.0); // peak − 30 dB, ~−90 dBFS minimum
        // Start: walk back while the previous frame is still speech-level → recover onset.
        let mut ns = s;
        while ns > 0 && rms[ns - 1] >= floor {
            ns -= 1;
        }
        // End: walk back while the previous frame is below floor → trim silent tail.
        let mut ne = e;
        while ne > ns + 1 && rms[ne - 1] < floor {
            ne -= 1;
        }
        if ne <= ns {
            ne = ns + 1;
        }
        seg.start_sample = ns * frame;
        seg.end_sample = (ne * frame).min(pcm.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `silence_ms` of zero, then `speech_ms` of a 220 Hz sine at `amp`, then `silence_ms` more.
    fn synthetic(rate: u32, silence_ms: u32, speech_ms: u32, amp: i16) -> Vec<i16> {
        let n = rate as usize * (silence_ms * 2 + speech_ms) as usize / 1000;
        let mut pcm = vec![0i16; n];
        let off = rate as usize * silence_ms as usize / 1000;
        let len = rate as usize * speech_ms as usize / 1000;
        for i in 0..len {
            let phase = (i as f64 * 2.0 * std::f64::consts::PI * 220.0 / rate as f64).sin();
            pcm[off + i] = (phase * amp as f64) as i16;
        }
        pcm
    }

    #[test]
    fn snap_recovers_onset_and_trims_tail() {
        let rate = 8000u32;
        let frame = 160usize; // 20 ms
        // 1 s silence + 0.5 s speech (amp 2000) + 1 s silence.
        let pcm = synthetic(rate, 1000, 500, 2000);
        let speech_off = rate as usize; // 1.0 s
        let speech_end = rate as usize * 3 / 2; // 1.5 s
        // Coarse: onset truncated by ~voice_ms (200 ms), tail padded by ~silence_ms (500 ms).
        let coarse_start = speech_off + frame * 10;
        let coarse_end = speech_end + frame * 25;
        let mut segs = vec![SpeechSegment {
            start_sample: coarse_start,
            end_sample: coarse_end,
        }];
        snap_segments(&pcm, frame, &mut segs);

        let s = &segs[0];
        assert!(
            s.start_sample <= coarse_start,
            "snap must not push start later"
        );
        assert!(
            s.start_sample.abs_diff(speech_off) <= frame,
            "start should be near onset {speech_off}, got {}",
            s.start_sample
        );
        assert!(s.end_sample <= coarse_end, "snap must not push end later");
        assert!(
            s.end_sample.abs_diff(speech_end) <= frame,
            "end should be near speech end {speech_end}, got {}",
            s.end_sample
        );
    }

    #[test]
    fn snap_keeps_degenerate_segments_sane() {
        // All-silence PCM: peak=0 → floor=1.0; a coarse segment over silence should collapse
        // to a single frame (ne clamped to ns+1) without panicking.
        let pcm = vec![0i16; 8000];
        let mut segs = vec![SpeechSegment {
            start_sample: 1600,
            end_sample: 3200,
        }];
        snap_segments(&pcm, 160, &mut segs);
        let s = &segs[0];
        assert!(s.end_sample >= s.start_sample);
    }
}
