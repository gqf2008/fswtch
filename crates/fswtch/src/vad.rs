//! Voice Activity Detection (VAD) — an owned wrapper with two selectable engines.
//!
//! A [`Vad`] detects speech in 16-bit PCM audio frames, emitting state transitions
//! (`StartTalking`, `Talking`, `StopTalking`) as audio is fed in via [`Vad::process`]. The engine
//! is chosen at construction time with [`VadEngine`]:
//!
//! - [`VadEngine::FreeSwitch`] — FreeSWITCH's built-in `switch_vad_t` (energy / fvad). Accepts
//!   8/16/32/48 kHz. The historical default.
//! - [`VadEngine::Earshot`] — [`earshot`](https://crates.io/crates/earshot), a pure-Rust neural
//!   VAD. Its model is trained at 16 kHz / 256-sample (16 ms) frames, so non-16 kHz input is
//!   transparently upsampled to 16 kHz with [`crate::Resample`] (FreeSWITCH's resampler); at 16 kHz
//!   the engine touches no FreeSWITCH symbol.
//!
//! Both engines share an identical API surface — [`Vad::process`] returns [`VadState`] transitions
//! for either — so callers can switch engines by changing one constructor argument.
//!
//! # Feature gating & inherent FS coupling
//!
//! The earshot engine (the `earshot` dependency, [`VadEngine::Earshot`], [`Vad`]'s earshot backend)
//! is gated behind the **`earshot`** feature (on by default). Disable it (`default-features = false`)
//! to ship only the FreeSwitch engine without compiling earshot.
//!
//! The FreeSwitch engine is always compiled, so `Vad` always references `switch_vad_*` symbols
//! (`with_engine`/`process`/`Drop` each carry the FreeSwitch arm in one function body). This means
//! the `Vad`-level earshot path is *not* FreeSWITCH-link-free: even an earshot-only `Vad` drags in
//! `switch_vad_init`/`switch_vad_process`/`switch_vad_destroy` via those shared method bodies. This
//! is inherent to unifying both engines behind one `Vad` + enum API (a deliberate tradeoff against a
//! per-engine-type or trait-object design with vtable indirection on the hot path). For a truly
//! FreeSWITCH-free neural VAD, use the [`earshot`](https://crates.io/crates/earshot) crate directly;
//! the `live_fs`-gated earshot integration tests document this constraint.

use std::ffi::CString;
use std::fmt;
use std::marker::PhantomData;
use std::ptr::NonNull;

use crate::{Result, SwitchError, cstring, sys};

/// Selects the VAD engine backing a [`Vad`].
///
/// Pass to [`Vad::with_engine`]; [`Vad::new`] uses [`VadEngine::default`] (FreeSwitch).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VadEngine {
    /// FreeSWITCH's built-in `switch_vad_t` (energy / fvad). Accepts 8/16/32/48 kHz.
    #[default]
    FreeSwitch,
    /// earshot — pure-Rust neural VAD. 16 kHz model; non-16 kHz input is resampled to 16 kHz.
    ///
    /// Only available with the `earshot` feature (on by default).
    #[cfg(feature = "earshot")]
    Earshot,
}

/// The outcome of feeding a frame of audio into [`Vad::process`], or the VAD's current state.
///
/// Wraps FreeSWITCH's `switch_vad_state_t`. The values mirror the `SWITCH_VAD_STATE_*`
/// constants exposed by `fswtch-sys`.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct VadState(pub(crate) sys::switch_vad_state_t);

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
    pub(crate) const fn from_raw(state: sys::switch_vad_state_t) -> Self {
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
/// Allocated with [`Vad::new`] (or [`Vad::with_engine`] to pick the engine) and destroyed on
/// [`Drop`]. Feed PCM frames in with [`process`](Self::process) and read the resulting [`VadState`].
///
/// `process` (and the other state-mutating methods) take `&mut self`: both engines mutate internal
/// state — FreeSwitch through its FFI handle, earshot through its detector + hysteresis. The type
/// is `!Send` / `!Sync`; use it from a single thread (or task) as FreeSWITCH's media thread model
/// already requires.
pub struct Vad {
    backend: VadBackend,
    // The *effective* rate (0 → 8000, matching `switch_vad_init`); used for frame math in
    // `speech_segments` and to drive the earshot resampler (rate != 16000 → upsample).
    sample_rate: u32,
    channels: i32,
    // Not thread-safe; both backends mutate state through `&mut self`.
    _marker: PhantomData<*const ()>,
}

/// The engine-specific state held by a [`Vad`].
enum VadBackend {
    /// FreeSWITCH `switch_vad_t`. Owned: `Drop` calls `switch_vad_destroy`.
    FreeSwitch(NonNull<sys::switch_vad_t>),
    /// earshot neural VAD. `inner` is pure-Rust (16 kHz); boxed because `earshot::Detector`
    /// (~8 KiB) would otherwise inflate this enum variant (clippy::large_enum_variant).
    /// `resampler` upsamples input → 16 kHz when `sample_rate != 16000` (FreeSWITCH's resampler).
    #[cfg(feature = "earshot")]
    Earshot {
        inner: Box<EarshotInner>,
        resampler: Option<crate::Resample>,
        /// Reused downmix scratch for `channels > 1` input.
        mono_scratch: Vec<i16>,
    },
}

impl Vad {
    /// Creates a new VAD using the default engine ([`VadEngine::FreeSwitch`]) for the given
    /// `sample_rate` (Hz) and `channels`.
    ///
    /// Equivalent to [`Vad::with_engine`]`(sample_rate, channels, VadEngine::default())`.
    pub fn new(sample_rate: i32, channels: i32) -> Result<Self> {
        Self::with_engine(sample_rate, channels, VadEngine::default())
    }

    /// Creates a new VAD backed by `engine` for the given `sample_rate` (Hz) and `channels`.
    ///
    /// `sample_rate` is typically `8000`, `16000`, `32000`, or `48000`; `channels` is usually
    /// `1` (mono). Returns [`crate::SwitchError`](`crate::GENERR`) if the FreeSwitch engine fails
    /// to allocate, or if the earshot engine cannot build its resampler.
    pub fn with_engine(sample_rate: i32, channels: i32, engine: VadEngine) -> Result<Self> {
        // `switch_vad_init` substitutes 8000 for a zero/negative rate; mirror that so the frame
        // math in `speech_segments` agrees with the rate the detector actually runs at.
        let rate = if sample_rate > 0 {
            sample_rate as u32
        } else {
            8000
        };
        let channels = if channels > 0 { channels } else { 1 };
        let backend = match engine {
            VadEngine::FreeSwitch => {
                // SAFETY: `switch_vad_init` is a plain allocator taking two ints; passing arbitrary
                // integers is sound (it returns NULL for invalid arguments).
                let raw = unsafe { sys::switch_vad_init(sample_rate as _, channels as _) };
                NonNull::new(raw)
                    .map(VadBackend::FreeSwitch)
                    .ok_or(SwitchError(crate::GENERR))?
            }
            #[cfg(feature = "earshot")]
            VadEngine::Earshot => {
                // 16 kHz → no resampling; any other rate → upsample to the 16 kHz the model needs.
                let resampler = if rate == EARSHOT_RATE {
                    None
                } else {
                    Some(crate::Resample::new(
                        rate,
                        EARSHOT_RATE,
                        1,
                        crate::DEFAULT_QUALITY,
                    )?)
                };
                VadBackend::Earshot {
                    inner: Box::new(EarshotInner::new()),
                    resampler,
                    mono_scratch: Vec::new(),
                }
            }
        };
        Ok(Self {
            backend,
            sample_rate: rate,
            channels,
            _marker: PhantomData,
        })
    }

    /// The raw `switch_vad_t` pointer for the FreeSwitch engine, or null for the earshot engine
    /// (which has no FreeSWITCH handle). Escape hatch for direct FFI; prefer the safe API.
    #[inline]
    pub fn as_ptr(&self) -> *mut sys::switch_vad_t {
        match &self.backend {
            VadBackend::FreeSwitch(raw) => raw.as_ptr(),
            #[cfg(feature = "earshot")]
            VadBackend::Earshot { .. } => std::ptr::null_mut(),
        }
    }

    /// Feeds one PCM frame to the VAD and returns the resulting state transition.
    ///
    /// `pcm` is a slice of signed 16-bit samples (`int16_t`). For the FreeSwitch engine,
    /// `samples` passed to the FFI is `pcm.len()` and the buffer may be read/written in place.
    /// For the earshot engine, `pcm` is downmixed to mono when `channels > 1`, resampled to
    /// 16 kHz when the configured rate is not 16 kHz, then scored in 256-sample (16 ms) frames;
    /// the returned [`VadState`] is the last frame's state (hysteresis-emitted `START_TALKING` /
    /// `TALKING` / `STOP_TALKING`, or `NONE`). Both engines honor the same single-frame contract:
    /// feed small frames (as [`speech_segments`](Self::speech_segments) does) to avoid missing a
    /// mid-buffer transition.
    ///
    /// # Buffer mutation (asymmetric across engines)
    ///
    /// The `&mut [i16]` borrow covers the worst case, but in-place mutation is engine/rate
    /// dependent: the FreeSwitch engine may write `pcm` (its C API takes `int16_t *`), and the
    /// earshot engine *only* when the configured rate is not 16 kHz (because
    /// [`Resample::process`](crate::Resample::process) takes a non-const source). At 16 kHz / mono
    /// the earshot engine merely reads `pcm`. Callers should not rely on `pcm` being preserved.
    pub fn process(&mut self, pcm: &mut [i16]) -> VadState {
        match &mut self.backend {
            VadBackend::FreeSwitch(raw) => {
                // SAFETY: `raw` is a live, owned VAD. `pcm.as_mut_ptr()`/`len()` describe a valid
                // mutable buffer for the duration of the call.
                let state = unsafe {
                    sys::switch_vad_process(
                        raw.as_ptr(),
                        pcm.as_mut_ptr(),
                        pcm.len() as sys::switch_vad_state_t,
                    )
                };
                VadState::from_raw(state)
            }
            #[cfg(feature = "earshot")]
            VadBackend::Earshot {
                inner,
                resampler,
                mono_scratch,
            } => {
                // 1. mono: pass through for mono input, else average channels into scratch.
                let mono: &mut [i16] = if self.channels <= 1 {
                    pcm
                } else {
                    downmix(pcm, self.channels as usize, mono_scratch);
                    mono_scratch.as_mut_slice()
                };
                // 2. feed at 16 kHz: direct, or via the resampler for non-16 kHz input.
                if self.sample_rate == EARSHOT_RATE {
                    inner.process_16k(mono)
                } else {
                    // SAFETY-ish: `resampler` is `Some` iff `sample_rate != 16000` (see `with_engine`).
                    let res = resampler
                        .as_ref()
                        .expect("earshot resampler allocated when rate != 16000");
                    let out16 = res.process(mono);
                    inner.process_16k(out16)
                }
            }
        }
    }

    /// Resets the VAD to its initial state, clearing any remembered speech/silence history.
    pub fn reset(&mut self) {
        match &mut self.backend {
            VadBackend::FreeSwitch(raw) => {
                // SAFETY: `raw` is a live VAD.
                unsafe { sys::switch_vad_reset(raw.as_ptr()) };
            }
            #[cfg(feature = "earshot")]
            VadBackend::Earshot { inner, .. } => inner.reset(),
        }
    }

    /// The VAD's current (most recently produced) state without feeding new audio.
    pub fn state(&self) -> VadState {
        match &self.backend {
            VadBackend::FreeSwitch(raw) => {
                // SAFETY: `raw` is a live VAD.
                let state = unsafe { sys::switch_vad_get_state(raw.as_ptr()) };
                VadState::from_raw(state)
            }
            #[cfg(feature = "earshot")]
            VadBackend::Earshot { inner, .. } => inner.state(),
        }
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
    /// For the earshot engine there is no fvad mode; `mode` is mapped to a score threshold
    /// instead (higher mode = stricter, higher threshold): `-1`/`0` → 0.50, `1` → 0.55,
    /// `2` → 0.60, `3`+ → 0.65.
    ///
    /// Returns [`crate::SwitchError`](`crate::GENERR`) on failure (FreeSwitch: non-zero return).
    pub fn set_mode(&mut self, mode: i32) -> Result<()> {
        match &mut self.backend {
            VadBackend::FreeSwitch(raw) => {
                // SAFETY: `raw` is a live VAD; `mode` is a plain integer.
                let rc = unsafe { sys::switch_vad_set_mode(raw.as_ptr(), mode as _) };
                if rc == 0 {
                    Ok(())
                } else {
                    Err(SwitchError(crate::GENERR))
                }
            }
            #[cfg(feature = "earshot")]
            VadBackend::Earshot { inner, .. } => {
                let threshold = match mode {
                    -1 | 0 => 0.50,
                    1 => 0.55,
                    2 => 0.60,
                    _ => 0.65, // 3 (very aggressive) and unknown
                };
                inner.set_threshold(threshold);
                Ok(())
            }
        }
    }

    /// Sets a named VAD parameter to an integer value.
    ///
    /// `key` is a NUL-free C string (interior NULs map to [`crate::SwitchError`](`crate::GENERR`)).
    /// For the FreeSwitch engine the value type is `int` (`switch_vad_set_param`). For the earshot
    /// engine, the recognized keys are:
    /// - `"voice_ms"` — onset required before `START_TALKING` (ms; both engines).
    /// - `"silence_ms"` — trailing silence before `STOP_TALKING` (ms; both engines).
    /// - `"threshold"` — earshot score threshold in **thousandths** (e.g. `500` → `0.5`).
    ///
    /// Unknown keys are accepted (no-op) and return `Ok(())`.
    ///
    /// # `threshold` vs `set_mode` — two scales for one knob
    ///
    /// earshot's score threshold can be set two ways and they use **different units**:
    /// [`set_mode`](Self::set_mode) sets it to an absolute float (0.50 / 0.55 / 0.60 / 0.65 by
    /// aggressiveness), while `set_param("threshold", v)` interprets `v` as **thousandths**
    /// (`500` → `0.5`). `set_mode` runs last-wins only if called after `set_param`. Mind the
    /// scale: `set_param("threshold", 2)` means `0.002`, not `0.2`.
    pub fn set_param(&mut self, key: impl AsRef<str>, val: i32) -> Result<()> {
        match &mut self.backend {
            VadBackend::FreeSwitch(raw) => {
                let key: CString = cstring(key)?;
                // SAFETY: `raw` is a live VAD; `key` is a valid null-terminated C string for the
                // duration of the call.
                unsafe { sys::switch_vad_set_param(raw.as_ptr(), key.as_ptr(), val as _) };
                // `switch_vad_set_param` returns void, so there is no status to map.
                Ok(())
            }
            #[cfg(feature = "earshot")]
            VadBackend::Earshot { inner, .. } => {
                match key.as_ref() {
                    "voice_ms" => inner.set_voice_ms(val.max(0) as u32),
                    "silence_ms" => inner.set_silence_ms(val.max(0) as u32),
                    "threshold" => inner.set_threshold((val as f32 / 1000.0).clamp(0.0, 1.0)),
                    _ => {} // ignored, mirroring FreeSwitch's void return for unknown keys
                }
                Ok(())
            }
        }
    }

    /// The sample rate this VAD runs at (the effective rate — `0` passed to [`new`](Self::new) is
    /// reported as `8000`, matching `switch_vad_init`).
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
    /// speech onset, trailing silence trimmed, using a per-segment floor of peak − 30 dB. The
    /// earshot engine applies the same hysteresis shape (configurable via [`set_param`]) and
    /// benefits identically from snapping.
    ///
    /// `frame_ms` must be a value the detector accepts (10/20/30 when fvad is enabled via
    /// [`set_mode`](Self::set_mode); any when on the native energy path; any for earshot, which
    /// re-frames internally to 16 ms). Segment bounds are in **samples**; use
    /// [`SpeechSegment::duration_ms`] / [`SpeechSegment::samples`].
    ///
    /// [`set_param`]: Self::set_param
    pub fn speech_segments(&mut self, pcm: &[i16], frame_ms: u32) -> Vec<SpeechSegment> {
        let frame = self.frame_samples(frame_ms);
        let mut segs = self.coarse_segments(pcm, frame);
        snap_segments(pcm, frame, &mut segs);
        segs
    }

    /// The raw hysteresis segments (before snapping): `START_TALKING`..`STOP_TALKING`
    /// transitions driven by [`process`](Self::process). Exposed so callers can apply their own
    /// post-processing; prefer [`speech_segments`](Self::speech_segments) for LLM/ASR slicing.
    fn coarse_segments(&mut self, pcm: &[i16], frame: usize) -> Vec<SpeechSegment> {
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
        // Only the FreeSwitch engine owns a C handle that must be destroyed; the earshot variant's
        // fields (detector / `Option<Resample>` / `Vec`) drop themselves (the Earshot arm is a
        // no-op, present only for exhaustiveness when the `earshot` feature is on).
        match &mut self.backend {
            VadBackend::FreeSwitch(raw) => {
                // SAFETY: `raw` owns exactly one `switch_vad_t`, and `switch_vad_destroy` takes the
                // pointer by reference (`*mut *mut`) so it can NULL it out; the box is not otherwise
                // touched after this point.
                let mut ptr = raw.as_ptr();
                unsafe { sys::switch_vad_destroy(&mut ptr) };
            }
            #[cfg(feature = "earshot")]
            VadBackend::Earshot { .. } => {}
        }
    }
}

impl fmt::Debug for Vad {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let engine = match &self.backend {
            VadBackend::FreeSwitch(_) => "freeswitch",
            #[cfg(feature = "earshot")]
            VadBackend::Earshot { .. } => "earshot",
        };
        f.debug_struct("Vad")
            .field("engine", &engine)
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels)
            .field("state", &self.state())
            .finish()
    }
}

/// earshot's model: 16 kHz, 256-sample (16 ms) frames.
#[cfg(feature = "earshot")]
const EARSHOT_RATE: u32 = 16_000;
#[cfg(feature = "earshot")]
const EARSHOT_FRAME: usize = 256;

/// Pure-Rust earshot VAD core: scores 16 kHz mono PCM in 256-sample frames and emits
/// [`VadState`] transitions via a FreeSWITCH-style hysteresis.
///
/// This type deliberately holds **no** FreeSWITCH handle (the resampler lives in
/// [`VadBackend::Earshot`], not here), so it carries no FFI symbols and is fully testable without
/// a linked FreeSWITCH.
#[cfg(feature = "earshot")]
struct EarshotInner {
    detector: earshot::Detector,
    /// 16 kHz mono staging buffer, drained in `EARSHOT_FRAME`-sample chunks.
    stage: Vec<i16>,
    /// Hysteresis state.
    in_speech: bool,
    onset_accum: u32,
    silence_accum: u32,
    state: VadState,
    /// Config (set via `Vad::set_param` / `set_mode`).
    threshold: f32,
    voice_samples: u32,
    silence_samples: u32,
}

#[cfg(feature = "earshot")]
impl EarshotInner {
    /// New core with FreeSWITCH-mirroring defaults: threshold 0.5, voice_ms 200, silence_ms 500.
    fn new() -> Self {
        Self {
            detector: earshot::Detector::default(),
            stage: Vec::with_capacity(EARSHOT_FRAME * 2),
            in_speech: false,
            onset_accum: 0,
            silence_accum: 0,
            state: VadState::NONE,
            threshold: 0.5,
            voice_samples: 200 * EARSHOT_RATE / 1000,
            silence_samples: 500 * EARSHOT_RATE / 1000,
        }
    }

    /// Feeds 16 kHz mono PCM, accumulating into 256-sample frames and running the hysteresis;
    /// returns the state of the last fully processed frame (or the current state if no full frame
    /// was completed this call).
    fn process_16k(&mut self, mono: &[i16]) -> VadState {
        self.stage.extend_from_slice(mono);
        let mut last = self.state;
        while self.stage.len() >= EARSHOT_FRAME {
            let score = self.detector.predict_i16(&self.stage[..EARSHOT_FRAME]);
            last = self.step(score);
            self.stage.drain(..EARSHOT_FRAME);
        }
        self.state = last;
        last
    }

    /// One hysteresis step given a 256-sample frame's score. Mirrors FreeSWITCH's
    /// `START_TALKING` → `TALKING` → `STOP_TALKING` semantics.
    ///
    /// # Onset sensitivity (validate before barge-in/turn use)
    ///
    /// During onset a single frame scoring below `threshold` **hard-resets** `onset_accum` to
    /// zero. Real neural VAD scores dip below threshold mid-utterance, so one 16 ms dip inside the
    /// `voice_ms` window discards all accumulated onset — making `START_TALKING` more jittery /
    /// later-firing than FreeSWITCH's gradual hangover. This is a deliberate, predictable
    /// simplification. Before relying on it for barge-in or turn detection, **validate against real
    /// speech clips** and tune `voice_ms` / `threshold` accordingly (or consider a decay-based
    /// onset if sustained robustness is needed).
    fn step(&mut self, score: f32) -> VadState {
        let voice = score >= self.threshold;
        if !self.in_speech {
            if voice {
                self.onset_accum += EARSHOT_FRAME as u32;
                if self.onset_accum >= self.voice_samples {
                    self.in_speech = true;
                    self.onset_accum = 0;
                    self.silence_accum = 0;
                    VadState::START_TALKING
                } else {
                    VadState::NONE
                }
            } else {
                // A non-voice frame resets the onset accumulator (simple, predictable hysteresis).
                self.onset_accum = 0;
                VadState::NONE
            }
        } else if voice {
            self.silence_accum = 0;
            VadState::TALKING
        } else {
            self.silence_accum += EARSHOT_FRAME as u32;
            if self.silence_accum >= self.silence_samples {
                self.in_speech = false;
                self.onset_accum = 0;
                self.silence_accum = 0;
                VadState::STOP_TALKING
            } else {
                VadState::TALKING
            }
        }
    }

    fn reset(&mut self) {
        self.detector.reset();
        self.stage.clear();
        self.in_speech = false;
        self.onset_accum = 0;
        self.silence_accum = 0;
        self.state = VadState::NONE;
    }

    fn state(&self) -> VadState {
        self.state
    }

    fn set_threshold(&mut self, threshold: f32) {
        self.threshold = threshold;
    }

    fn set_voice_ms(&mut self, voice_ms: u32) {
        self.voice_samples = voice_ms * EARSHOT_RATE / 1000;
    }

    fn set_silence_ms(&mut self, silence_ms: u32) {
        self.silence_samples = silence_ms * EARSHOT_RATE / 1000;
    }
}

/// Downmixes interleaved multi-channel `pcm` to mono by averaging each frame into `out`
/// (cleared and refilled). A trailing partial frame (`pcm.len() % channels != 0`) is dropped.
#[cfg(feature = "earshot")]
fn downmix(pcm: &[i16], channels: usize, out: &mut Vec<i16>) {
    if channels == 0 {
        return;
    }
    let nframes = pcm.len() / channels;
    out.clear();
    out.reserve(nframes);
    for f in 0..nframes {
        let s = f * channels;
        let mut sum = 0i32;
        for c in 0..channels {
            sum += pcm[s + c] as i32;
        }
        out.push((sum / channels as i32) as i16);
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

    /// A more speech-like fixture than [`synthetic`]: a voiced source (sum of harmonics with 1/k
    /// decay, ~120 Hz f₀) shaped by an onset/offset envelope plus low-level deterministic noise
    /// for the broadband content real speech carries. earshot is trained on real speech, so a pure
    /// 220 Hz tone may not score as voice — this raises the chance the neural VAD fires.
    ///
    /// Still synthetic; the `live_fs` earshot tests using it must be **confirmed on a real
    /// FreeSWITCH build** (they cannot run in the headers-only default build).
    #[cfg(all(feature = "earshot", feature = "live_fs"))]
    fn synthetic_speech(rate: u32, silence_ms: u32, speech_ms: u32, amp: i16) -> Vec<i16> {
        let n = rate as usize * (silence_ms * 2 + speech_ms) as usize / 1000;
        let mut pcm = vec![0i16; n];
        let off = rate as usize * silence_ms as usize / 1000;
        let len = rate as usize * speech_ms as usize / 1000;
        let f0 = 120.0; // Hz — typical male voice
        let ramp = (rate as usize / 20).min(len / 4); // ~50 ms onset/offset ramp
        // Deterministic LCG noise so the fixture is reproducible.
        let mut rng: u32 = 0x5EED;
        for i in 0..len {
            let t = i as f64 / rate as f64;
            let mut s = 0.0;
            for k in 1..=6_u32 {
                let amp_k = amp as f64 / k as f64;
                s += amp_k * (2.0 * std::f64::consts::PI * f0 * k as f64 * t).sin();
            }
            s /= 6.0; // normalize the harmonic sum
            // onset/offset envelope.
            let env = if i < ramp {
                i as f64 / ramp as f64
            } else if i > len.saturating_sub(ramp) {
                ((len - i) as f64 / ramp as f64).max(0.0)
            } else {
                1.0
            };
            s *= env;
            // ~5% broadband noise (breath/frication).
            rng = rng.wrapping_mul(1103515245).wrapping_add(12345);
            let noise = (rng as f64 / u32::MAX as f64 * 2.0 - 1.0) * (amp as f64 * 0.05);
            s += noise;
            pcm[off + i] = s.round().clamp(-32768.0, 32767.0) as i16;
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

    #[test]
    fn vadengine_default_is_freeswitch() {
        assert_eq!(VadEngine::default(), VadEngine::FreeSwitch);
    }

    /// Drives the hysteresis directly (no `predict_i16`) so it is deterministic and FreeSWITCH-
    /// link-free. Verifies the `NONE* → START_TALKING → TALKING* → STOP_TALKING → NONE*` shape
    /// and that each transition fires at exactly its configured threshold.
    #[cfg(feature = "earshot")]
    #[test]
    fn earshot_hysteresis_start_and_stop() {
        let mut inner = EarshotInner::new();
        // ceil(threshold_samples / frame): the frame at which the accumulator first crosses it.
        let voice_frames = (inner.voice_samples as usize).div_ceil(EARSHOT_FRAME);
        let silence_frames = (inner.silence_samples as usize).div_ceil(EARSHOT_FRAME);

        // Voice phase: NONE while onset accumulates, then exactly one START, then TALKING.
        let mut states: Vec<VadState> = (0..voice_frames + 3).map(|_| inner.step(0.9)).collect();
        let start_idx = states
            .iter()
            .position(|&s| s == VadState::START_TALKING)
            .expect("START_TALKING must fire");
        assert_eq!(
            states
                .iter()
                .filter(|&&s| s == VadState::START_TALKING)
                .count(),
            1,
            "exactly one START_TALKING"
        );
        assert!(
            states[..start_idx].iter().all(|&s| s == VadState::NONE),
            "pre-onset frames must be NONE"
        );
        assert!(
            states[start_idx + 1..]
                .iter()
                .all(|&s| s == VadState::TALKING),
            "post-onset frames must be TALKING"
        );
        assert_eq!(
            start_idx + 1,
            voice_frames,
            "START fires at the onset threshold"
        );

        // Silence phase: TALKING while silence accumulates, then exactly one STOP, then NONE.
        states = (0..silence_frames + 3).map(|_| inner.step(0.1)).collect();
        let stop_idx = states
            .iter()
            .position(|&s| s == VadState::STOP_TALKING)
            .expect("STOP_TALKING must fire");
        assert_eq!(
            states
                .iter()
                .filter(|&&s| s == VadState::STOP_TALKING)
                .count(),
            1,
            "exactly one STOP_TALKING"
        );
        assert!(
            states[..stop_idx].iter().all(|&s| s == VadState::TALKING),
            "pre-stop frames must be TALKING"
        );
        assert!(
            states[stop_idx + 1..].iter().all(|&s| s == VadState::NONE),
            "post-stop frames must be NONE"
        );
        assert_eq!(
            stop_idx + 1,
            silence_frames,
            "STOP fires at the silence threshold"
        );

        // After STOP, a voice frame restarts onset accumulation from zero (NONE until threshold).
        assert_eq!(inner.step(0.9), VadState::NONE, "onset restarts from zero");
    }

    /// Feeding 16 kHz silence must never spuriously report speech. Pure-Rust, no FreeSWITCH link.
    #[cfg(feature = "earshot")]
    #[test]
    fn earshot_silence_is_none() {
        let mut inner = EarshotInner::new();
        // 1 s of 16 kHz silence, fed in 256-sample chunks (earshot's frame size).
        for _ in 0..(EARSHOT_RATE as usize / EARSHOT_FRAME) {
            inner.process_16k(&[0i16; EARSHOT_FRAME]);
        }
        assert_eq!(
            inner.state(),
            VadState::NONE,
            "silence must not start speech"
        );
    }

    #[cfg(all(feature = "earshot", feature = "live_fs"))]
    #[test]
    fn earshot_vad_speech_segments() {
        // 0.5 s silence + 1 s multi-harmonic "speech" + 0.5 s silence, 16 kHz.
        // NOTE: earshot is speech-trained; confirm this fixture fires on a real FreeSWITCH build.
        let pcm = synthetic_speech(EARSHOT_RATE, 500, 1000, 12_000);
        let mut vad = Vad::with_engine(EARSHOT_RATE as i32, 1, VadEngine::Earshot)
            .expect("earshot vad at 16 kHz");
        let segs = vad.speech_segments(&pcm, 16);
        assert!(!segs.is_empty(), "earshot should detect speech segments");
        for s in &segs {
            assert!(s.end_sample > s.start_sample);
        }
    }

    #[cfg(all(feature = "earshot", feature = "live_fs"))]
    #[test]
    fn earshot_vad_resampled_8k() {
        // 8 kHz input exercises the earshot resampler path (pipeline 8k → model 16k).
        // NOTE: confirm on a real FreeSWITCH build.
        let pcm = synthetic_speech(8000, 500, 1000, 12_000);
        let mut vad = Vad::with_engine(8000, 1, VadEngine::Earshot).expect("earshot vad at 8 kHz");
        // Feed in 20 ms frames; we simply assert it processes without panicking and the VAD
        // reports some transition over a speechy buffer.
        let frame = 160usize; // 20 ms @ 8 kHz
        let mut any = false;
        let mut off = 0;
        while off < pcm.len() {
            let n = frame.min(pcm.len() - off);
            let mut buf = vec![0i16; frame];
            buf[..n].copy_from_slice(&pcm[off..off + n]);
            let st = vad.process(&mut buf);
            if st != VadState::NONE {
                any = true;
            }
            off += frame;
        }
        assert!(any, "expected at least one non-NONE state over speech");
    }

    #[cfg(feature = "live_fs")]
    #[test]
    fn freeswitch_vad_smoke() {
        let mut vad = Vad::new(16_000, 1).expect("freeswitch vad");
        let pcm = synthetic(16_000, 500, 1000, 12_000);
        let segs = vad.speech_segments(&pcm, 20);
        // FreeSwitch's energy VAD should reliably catch a loud 220 Hz tone.
        assert!(!segs.is_empty(), "freeswitch vad should detect the tone");
    }
}
