//! Endpoint I/O callbacks + per-call state for ai_agent_seat.
//!
//! FreeSWITCH drives this module as an **endpoint interface** (not an
//! application): the [`fswtch::EndpointIoRoutines`] trait implementation
//! [`AiAgent`] supplies `read_frame`, `write_frame`, `kill_channel`, and
//! `outgoing_channel` safe methods. fswtch's generic trampolines adapt these
//! to the `switch_io_routines` C function-pointer table that FreeSWITCH
//! invokes on its media thread at 50 Hz (20 ms frames).
//!
//! The callbacks receive **no `user_data`** parameter, so per-call state is
//! recovered via the session UUID → [`CallState`] lookup in the global
//! [`CALLS`] [`DashMap`]. A call enters the map when `write_frame` first sees
//! a session (lazy init via [`actor::init_call`]) and leaves it in
//! `kill_channel`.
//!
//! # Frame semantics
//!
//! - `write_frame(session, frame)`: FreeSWITCH writes the CALLER'S audio TO
//!   this endpoint. VAD runs here. The frame is read-only.
//! - `read_frame(session, frame)`: FreeSWITCH reads audio FROM this endpoint.
//!   We drain [`CallState::tts_cons`] (SPSC ringbuf) into the frame; silence
//!   when empty.
//! - `kill_channel(session, sig)`: call ended — drop the [`CallState`].
//! - `outgoing_channel(...)`: create the B leg when the dialplan bridges to
//!   `ai_agent/<num>`.
//!
//! Every trampoline is wrapped in `catch_unwind` by fswtch so a Rust panic
//! degrades to a logged error + `SWITCH_STATUS_FALSE` instead of unwinding
//! into FreeSWITCH.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use dashmap::DashMap;
use fswtch::{
    CallerProfile, EndpointInterfaceRef, EndpointIoRoutines, Frame, FrameMut, OriginateFlag,
    OutgoingResult, SUCCESS, Session, Status, request_session,
};
// `Consumer` trait is required for `ringbuf::Cons::pop_slice` / `clear`
// method resolution (they are trait methods, not inherent).
use ringbuf::traits::Consumer;

use crate::audio_dsp::{PIPELINE_SAMPLE_RATE, VAD_SAMPLE_RATE, get_codec_rate};
use crate::voice_core::Config;

/// earshot predicts on fixed 256-sample (16 ms at 16 kHz) frames.
const EARSHOT_FRAME_SIZE: usize = 256;

/// Higher threshold for barge-in detection (AI speaking). Suppresses residual
/// echo leakage that FS preprocess AEC didn't fully eliminate — real caller
/// voice scores >0.8, echo typically 0.5-0.7.
const BARGE_IN_SCORE_THRESHOLD: f32 = 0.8;

/// RMS energy of a frame, normalized to [0.0, 1.0] (full-scale i16 = 1.0).
/// Used as the noise gate — frames below `min_speech_rms` are treated as
/// silence regardless of VAD score. Integer sum-of-squares (i32 squared into
/// u64) avoids the f64 powi path on the per-frame hot path.
fn rms(frame: &[i16]) -> f32 {
    let sum_sq: u64 = frame.iter().map(|&s| (s as i64 * s as i64) as u64).sum();
    ((sum_sq as f64 / frame.len() as f64).sqrt() / 32768.0) as f32
}

/// Score one earshot frame: (score, rms). Shared by the barge-in and normal
/// VAD paths so the predict + RMS computation lives in one place.
fn score_frame(
    detector: Option<&mut earshot::Detector>,
    frame: &[i16; EARSHOT_FRAME_SIZE],
    min_rms: f32,
) -> (f32, f32) {
    // RMS-first gate: the neural predict is ~100-1000x more expensive than the
    // integer RMS sum. For a 1000-concurrent-call load where most calls are
    // silent at any moment, skipping predict on sub-threshold frames cuts VAD
    // CPU by an order of magnitude. The cheap RMS gate is the primary noise
    // filter anyway — if there's no energy, there's no speech to score.
    let rms = rms(frame);
    if rms < min_rms {
        return (-1.0, rms);
    }
    let score = match detector {
        Some(d) => d.predict_i16(frame),
        None => -1.0,
    };
    (score, rms)
}

/// Per-call state, owned by the global [`CALLS`] map and borrowed by the I/O
/// callbacks + the orchestrator's spawned tasks.
///
/// The SPSC ringbuf (`tts_cons`) is the bridge from the TTS driver loop
/// (async, tokio runtime — sole `Producer` via `on_audio`) to `read_frame`
/// (sync, FS media thread — sole `Consumer`): the driver pushes 8 kHz i16
/// PCM, `read_frame` drains it. A [`DashMap`] entry's `RefMut` gives us the
/// exclusive borrow `read_frame` needs without a separate lock.
///
/// `ai_speaking` is shared by value (Arc-clone) with the orchestrator so it
/// can set/clear it without touching the map.
pub struct CallState {
    pub uuid: String,
    /// earshot VAD detector (16 kHz). `None` when the pipeline rate is not 16 kHz.
    pub vad: Option<earshot::Detector>,
    /// Upsampler from the session's codec rate to 16 kHz, when they differ.
    pub(crate) resampler: Option<SendResample>,
    /// Pre-roll buffer (300 ms at 16 kHz) captured before speech onset.
    pub pre_roll: VecDeque<i16>,
    /// Fixed max size of `pre_roll` (samples). NOT `VecDeque::capacity()`, which
    /// grows on reallocation. 300 ms at 8 kHz = 2400.
    pub pre_roll_max: usize,
    /// Whether we are currently inside a speech segment.
    pub in_speech: bool,
    /// Onset accumulator (ms). Increases by frame_ms when score > threshold,
    /// decays by `speech_onset_decay` fraction when score < threshold.
    /// Speech onset fires when accumulator >= `speech_onset_ms`.
    pub onset_accum_ms: f32,
    /// Config-driven thresholds (from VadConfig via voice_seat.yaml).
    pub speech_threshold: f32,
    pub speech_onset_ms: f32,
    pub speech_onset_decay: f32,
    pub min_speech_rms: f32,
    /// Accumulated 16 kHz i16 PCM for the current speech segment.
    pub speech_buffer: Vec<i16>,
    /// Staging buffer for accumulating samples into earshot-sized (256) frames.
    pub vad_stage: Vec<i16>,
    /// Reusable scratch buffer for the 8 kHz → 16 kHz VAD bypass resampler.
    /// `fswtch::Resample::process` takes `&mut [i16]`, but `frame.pcm_i16()`
    /// yields `&[i16]` — without this scratch we'd allocate a fresh `Vec` on
    /// every 20 ms `write_frame` (50 Hz × 1000 calls = 50 k alloc/s). Cleared
    /// and refilled each frame; capacity stabilizes after the first frame.
    pub resample_scratch: Vec<i16>,
    /// Silence-timeout in samples (16 kHz). When `current_silence` exceeds
    /// this, the speech segment is considered complete.
    pub silence_samples: u32,
    /// Current silence sample count within a speech segment.
    pub current_silence: u32,
    /// Barge-in confirmation threshold in samples.
    pub barge_in_confirm_samples: u32,
    /// Current barge-in confirmation counter.
    pub barge_in_confirm: u32,
    /// TTS audio to play toward the caller (8 kHz i16 PCM). Drained by
    /// `read_frame`; filled by the TTS driver loop via the `on_audio` callback
    /// that owns the ringbuf `Producer`. This is a SPSC ring buffer (driver =
    /// sole producer on the tokio runtime, `read_frame` = sole consumer on the
    /// FS media thread), so the DashMap `RefMut` exclusive borrow is sufficient
    /// — no extra lock. `None` only transiently between `CallState::new` and
    /// `actor::init_call` assigning the consumer half.
    ///
    /// We use the `Direct` (`ringbuf::Cons`) wrapper rather than the `Caching`
    /// (`HeapCons`) one: `Direct` holds the `Arc<HeapRb<i16>>` directly and is
    /// `Send + Sync` (a hard requirement — `CallState` lives in a global
    /// `DashMap` `static`), whereas `Caching` uses `Cell` caches and is
    /// `!Sync`. The producer/consumer caches are unnecessary for our SPSC
    /// access pattern anyway.
    pub tts_cons: Option<ringbuf::Cons<std::sync::Arc<ringbuf::HeapRb<i16>>>>,
    /// Per-call speech-segment channel. `write_frame` (sync media thread) does
    /// a non-blocking `try_send` here at end-of-speech; the CallActor consumes
    /// it via `attach_stream` (`StreamMessage<Vec<i16>>`). Replaces the old
    /// per-segment `runtime::spawn(tell)` — zero spawn, backpressure is "drop
    /// the segment if the mailbox is full". `None` only transiently until
    /// `actor::init_call` assigns it.
    pub speech_tx: Option<tokio::sync::mpsc::Sender<Vec<i16>>>,
    /// AI-speaking flag, shared (Arc-clone) with the orchestrator.
    pub ai_speaking: Arc<AtomicBool>,
    /// Loaded configuration snapshot (may be `None` → defaults).
    pub config: Option<Config>,
    /// Per-call actor ref (set by `actor::init_call`). `None` only transiently
    /// during `CallState::new` before `init_call` assigns it.
    pub actor: Option<kameo::actor::ActorRef<crate::actor::CallActor>>,
}

impl CallState {
    /// Build a fresh `CallState` for `uuid` with the given codec rate and config.
    ///
    /// `codec_rate` is the session's read-codec sample rate (Hz); the resampler
    /// is created when it differs from [`PIPELINE_SAMPLE_RATE`].
    pub fn new(uuid: String, codec_rate: u32, config: Option<Config>) -> anyhow::Result<Self> {
        // VAD bypass resampler: pipeline 8 kHz → VAD 16 kHz (earshot requires
        // 16 kHz). Only used to feed VAD prediction; speech segment data stays
        // at pipeline rate (8 kHz).
        let resampler = if PIPELINE_SAMPLE_RATE != VAD_SAMPLE_RATE {
            Some(SendResample(
                fswtch::Resample::new(PIPELINE_SAMPLE_RATE, VAD_SAMPLE_RATE, 1, 1)
                    .map_err(|e| anyhow::anyhow!("VAD resample init: {e:?}"))?,
            ))
        } else {
            None
        };

        let vad = if VAD_SAMPLE_RATE == 16000 {
            Some(earshot::Detector::default())
        } else {
            None
        };

        let pre_roll_size = (PIPELINE_SAMPLE_RATE * 300 / 1000) as usize;

        let vad_config = config.as_ref().map(|c| c.vad.clone()).unwrap_or_default();
        let silence_samples = vad_config.silence_timeout_ms * VAD_SAMPLE_RATE / 1000;
        let barge_in_confirm_samples = vad_config.barge_in_confirm_ms * VAD_SAMPLE_RATE / 1000;

        Ok(Self {
            uuid,
            vad,
            resampler,
            pre_roll: VecDeque::with_capacity(pre_roll_size),
            pre_roll_max: pre_roll_size,
            in_speech: false,
            onset_accum_ms: 0.0,
            speech_threshold: vad_config.speech_threshold,
            speech_onset_ms: vad_config.speech_onset_ms,
            speech_onset_decay: vad_config.speech_onset_decay,
            min_speech_rms: vad_config.min_speech_rms,
            speech_buffer: Vec::new(),
            vad_stage: Vec::with_capacity(EARSHOT_FRAME_SIZE),
            resample_scratch: Vec::new(),
            silence_samples,
            current_silence: 0,
            barge_in_confirm_samples,
            barge_in_confirm: 0,
            tts_cons: None,
            speech_tx: None,
            ai_speaking: Arc::new(AtomicBool::new(false)),
            config,
            actor: None,
        })
    }

    /// Drop any buffered TTS audio (barge-in flush). Clears the SPSC ringbuf
    /// consumer so `read_frame` emits silence on the next frame.
    pub fn clear_tts(&mut self) {
        if let Some(cons) = self.tts_cons.as_mut() {
            cons.clear();
        }
    }
}

/// Global registry: call UUID → per-call state.
///
/// Inserted lazily by `write_frame` (via [`actor::init_call`]) on first frame,
/// removed by `kill_channel`. The DashMap shards across cores so the 50 Hz
/// media-thread lookups don't contend.
pub static CALLS: std::sync::LazyLock<DashMap<String, CallState>> =
    std::sync::LazyLock::new(DashMap::new);

/// One-shot flag: set when `write_frame` first receives a non-empty caller
/// frame, so the first media frame logs at INFO (subsequent frames are TRACE).
/// Reset to `false` on module unload (shutdown clears all calls).
static WRITE_FRAME_SEEN: AtomicBool = AtomicBool::new(false);

/// Global `write_frame` invocation counter, for periodic (1/sec) diagnostics.
static WF_FRAME_COUNT: AtomicU64 = AtomicU64::new(0);

// The `Send + Sync` resampler wrapper lives in `audio_dsp` (shared with
// `tts.rs`). The VAD bypass resampler (8 kHz → 16 kHz) is held here.
use crate::audio_dsp::SendResample;

/// The `ai_agent` endpoint: a unit-struct implementing
/// [`fswtch::EndpointIoRoutines`]. Behavior is fixed per-type; per-call state
/// lives in the global [`CALLS`] map keyed by session UUID (endpoints receive
/// no `user_data`).
pub struct AiAgent;

impl EndpointIoRoutines for AiAgent {
    const NAME: &'static str = "ai_agent";

    /// Create the B leg when FreeSWITCH bridges to `ai_agent/<num>`.
    ///
    /// Requests a new session on our endpoint, installs the caller profile +
    /// channel name, marks the channel answered (so the originator's bridge
    /// completes), sets `CF_AUDIO`, and initializes [`CallState`].
    ///
    /// We do **not** call `switch_core_session_thread_launch` —
    /// `switch_ivr_originate` does that after we return `CAUSE_SUCCESS`. We
    /// also do **not** force a channel state: `mark_answered` + the standard
    /// state handlers drive the leg to a media-exchange state where
    /// `read_frame` / `write_frame` fire.
    fn outgoing_channel(
        _session: Option<&Session>,
        caller_profile: Option<CallerProfile>,
        endpoint: &EndpointInterfaceRef,
        flags: OriginateFlag,
    ) -> OutgoingResult {
        // Request a new session on our endpoint (null UUID → FS generates one).
        let Some(new_session) = request_session(endpoint, fswtch::CallDirection::OUTBOUND, flags)
        else {
            tracing::error!("outgoing_channel: session request failed");
            return OutgoingResult::refused();
        };

        let Some(channel) = new_session.channel() else {
            tracing::error!("outgoing_channel: get_channel returned null");
            return OutgoingResult::refused();
        };

        // Install caller profile + name on the new channel.
        if let Some(ref profile) = caller_profile {
            channel.set_caller_profile(profile);
        }
        let _ = channel.set_name("ai_agent");

        // Mark answered so the A leg's bridge completes, and flag audio media
        // so FS wires our read/write_frame callbacks into the bridge.
        let _ = channel.mark_answered();
        channel.set_audio_flag();

        // Initialize read + write codecs (L16 at the pipeline's 8 kHz, 20 ms).
        if let Err(e) = new_session.init_read_codec("L16", PIPELINE_SAMPLE_RATE, 20, 1) {
            tracing::warn!("outgoing_channel: init_read_codec failed: {e}");
        }
        if let Err(e) = new_session.init_write_codec("L16", PIPELINE_SAMPLE_RATE, 20, 1) {
            tracing::warn!("outgoing_channel: init_write_codec failed: {e}");
        }

        // Drive the state machine out of CS_NEW. Without an explicit
        // `set_state`, the session thread sits in CS_NEW and FreeSWITCH
        // abandons it after its grace period (WRONG_CALL_STATE). CS_CONSUME_MEDIA
        // is the terminal "wait for media" state: our NULL state-handler table
        // lets the standard on_consume_media handler run (a no-op log), then
        // the thread sleeps on its condvar until hangup — exactly what a
        // synthesized B leg needs. The A leg's bridge then drives read/write
        // frames through our I/O routines.
        channel.set_state(fswtch::ChannelState::CONSUME_MEDIA);

        // Initialize per-call state.
        let uuid = channel.uuid().unwrap_or_default();
        if !uuid.is_empty() {
            let codec_rate = get_codec_rate(&new_session).max(8000);
            if let Err(e) = crate::actor::init_call(&uuid, codec_rate) {
                tracing::warn!("outgoing_channel: init_call for {uuid} failed: {e}");
            }
        }

        tracing::info!("outgoing_channel: created session {uuid} on endpoint ai_agent");
        OutgoingResult::success(new_session)
    }

    /// `write_frame`: FreeSWITCH writes the CALLER'S audio TO this endpoint.
    ///
    /// Runs VAD on the caller's speech; when a speech segment completes
    /// (silence timeout), spawns an orchestrator turn on the tokio runtime.
    /// The frame is NOT modified.
    fn write_frame(session: &Session, frame: &Frame) -> Status {
        let Some(uuid) = session.channel().and_then(|c| c.uuid()) else {
            return SUCCESS;
        };

        let samples = frame.pcm_i16();
        // Per-frame diagnostics: a global counter logs one line per second
        // (every 50 frames) showing raw sample count + accumulated VAD-stage
        // length, so we can tell whether write_frame is called steadily and
        // whether samples are reaching the VAD buffer. The first non-empty
        // frame also logs at INFO.
        let raw_len = samples.map(|s| s.len()).unwrap_or(0);
        let n = WF_FRAME_COUNT.fetch_add(1, Ordering::Relaxed);
        if !WRITE_FRAME_SEEN.load(Ordering::Relaxed) && raw_len > 0 {
            WRITE_FRAME_SEEN.store(true, Ordering::Relaxed);
            tracing::info!("write_frame: first media frame for {uuid} ({raw_len} samples)");
        }
        if n.is_multiple_of(50) {
            tracing::trace!(
                target: "ai_agent_seat::io",
                "write_frame #{n} {uuid}: raw_len={raw_len}"
            );
        }

        // Lazy-init the call on first frame (codec rate known by now).
        if !CALLS.contains_key(&uuid) {
            let codec_rate = get_codec_rate(session);
            if let Err(e) = crate::actor::init_call(&uuid, codec_rate) {
                tracing::warn!("init_call for {uuid} failed: {e}");
                return SUCCESS;
            }
        }

        let Some(mut state) = CALLS.get_mut(&uuid) else {
            return SUCCESS;
        };
        let state_mut = state.value_mut();

        let Some(samples) = samples else {
            return SUCCESS;
        };
        if samples.is_empty() {
            return SUCCESS;
        }

        // Accumulate raw 8 kHz caller PCM into speech_buffer (during speech) or
        // pre_roll (during silence). This is decoupled from the VAD loop (which
        // runs in the 16 kHz bypass domain) — VAD has ~16 ms latency, so the
        // onset is covered by pre_roll and trailing silence is trimmed by the
        // silence timeout. speech_buffer stays at pipeline rate (8 kHz) for the
        // LLM WAV, avoiding a lossy 16k→8k round-trip.
        if state_mut.in_speech {
            state_mut.speech_buffer.extend_from_slice(samples);
            // Diagnostic: log when speech_buffer grows beyond 2 seconds.
            // This should NOT happen for normal speech segments (< 10s).
            // If it does, the buffer is leaking across turns.
            if state_mut.speech_buffer.len() == 16000 {
                tracing::warn!(
                    "VAD {uuid}: speech_buffer reached {} samples (2s) — possible accumulation bug",
                    state_mut.speech_buffer.len()
                );
            }
        } else {
            state_mut.pre_roll.extend(samples.iter().copied());
            // Trim to a FIXED size — NOT `capacity()`. VecDeque reallocates on
            // overflow, so `capacity()` grows and the trim threshold grows with
            // it, turning pre_roll into an unbounded buffer (saw 94720 samples =
            // 11.8s instead of the intended 2400 = 300ms).
            while state_mut.pre_roll.len() > state_mut.pre_roll_max {
                state_mut.pre_roll.pop_front();
            }
        }

        // VAD bypass: upsample 8 kHz → 16 kHz for earshot prediction only.
        // VAD bypass: upsample 8 kHz → 16 kHz for earshot prediction only.
        // Reuse `resample_scratch` instead of allocating a fresh Vec every
        // frame — `process` takes `&mut [i16]` and reuses the buffer in place,
        // so we `clear()` + `extend` once per frame. The capacity stabilizes
        // after the first frame, so this is allocation-free on the steady path.
        // When no resampling is needed (rates match), extend vad_stage directly
        // from the input slice — zero alloc.
        if let Some(resampler) = state_mut.resampler.as_ref() {
            state_mut.resample_scratch.clear();
            state_mut.resample_scratch.extend_from_slice(samples);
            let out = resampler.0.process(&mut state_mut.resample_scratch);
            state_mut.vad_stage.extend_from_slice(out);
        } else {
            state_mut.vad_stage.extend_from_slice(samples);
        }

        // VAD: accumulate into 256-sample 16 kHz frames and score each.
        // Skip VAD entirely while AI is speaking — the TTS output leaks back
        // through the caller's mic (echo) and scores 0.8+ on earshot, causing
        // false speech segments. Half-duplex is simpler and more reliable than
        // trying to correlate echo vs real speech. VAD resumes automatically
        // when ai_speaking transitions to false (TTS done).
        let ai_speaking = state_mut.ai_speaking.load(Ordering::Relaxed);
        if ai_speaking {
            // AI is speaking — run barge-in detection with higher threshold.
            // Upsample + predict earshot, require sustained voice (config-driven
            // `barge_in_confirm_ms`) before interrupting. This prevents coughs,
            // echo leakage, and background noise from triggering false barge-ins.
            while state_mut.vad_stage.len() >= EARSHOT_FRAME_SIZE {
                let frame_slice: [i16; EARSHOT_FRAME_SIZE] = {
                    let mut buf = [0i16; EARSHOT_FRAME_SIZE];
                    buf.copy_from_slice(&state_mut.vad_stage[..EARSHOT_FRAME_SIZE]);
                    buf
                };
                state_mut.vad_stage.drain(..EARSHOT_FRAME_SIZE);

                let (score, rms) = score_frame(
                    state_mut.vad.as_mut(),
                    &frame_slice,
                    state_mut.min_speech_rms,
                );
                let is_voiced =
                    score >= BARGE_IN_SCORE_THRESHOLD && rms >= state_mut.min_speech_rms;

                let frame_len = frame_slice.len() as u32;
                if is_voiced {
                    state_mut.barge_in_confirm += frame_len;
                    if state_mut.barge_in_confirm >= state_mut.barge_in_confirm_samples {
                        tracing::info!(
                            "VAD: barge-in confirmed for {uuid} (score={score:.3} rms={rms:.4} \
                             confirm={}/{}samples)",
                            state_mut.barge_in_confirm,
                            state_mut.barge_in_confirm_samples
                        );
                        state_mut.barge_in_confirm = 0;
                        // Tell the actor to cancel the current turn + flush TTS.
                        if let Some(actor) = state_mut.actor.as_ref() {
                            let actor = actor.clone();
                            crate::runtime::spawn(async move {
                                let _ = actor.tell(crate::actor::BargeIn).await;
                            });
                        }
                    }
                } else {
                    state_mut.barge_in_confirm = 0;
                }
            }
            return SUCCESS;
        }

        while state_mut.vad_stage.len() >= EARSHOT_FRAME_SIZE {
            let frame_slice: [i16; EARSHOT_FRAME_SIZE] = {
                let mut buf = [0i16; EARSHOT_FRAME_SIZE];
                buf.copy_from_slice(&state_mut.vad_stage[..EARSHOT_FRAME_SIZE]);
                buf
            };
            state_mut.vad_stage.drain(..EARSHOT_FRAME_SIZE);

            let (score, rms) = score_frame(
                state_mut.vad.as_mut(),
                &frame_slice,
                state_mut.min_speech_rms,
            );
            // RMS noise gate: skip frames with low energy regardless of VAD
            // score. This is the primary defense against background noise —
            // speexdsp in the dialplan preprocess reduces noise, and the RMS
            // gate catches what's left. Configured via `min_speech_rms` in yaml.
            // (ai_speaking is already handled by the early-return branch above,
            // so here the normal speech_threshold always applies.)
            let rms_gate = rms >= state_mut.min_speech_rms;
            let threshold = state_mut.speech_threshold;

            // Onset accumulator (voice-call style): accumulate ms when score
            // > threshold AND RMS gate passes; decay when it doesn't. Speech
            // onset fires when accumulator >= speech_onset_ms.
            let frame_ms = EARSHOT_FRAME_SIZE as f32 * 1000.0 / VAD_SAMPLE_RATE as f32;
            let is_voiced = score >= threshold && rms_gate;
            if is_voiced {
                state_mut.onset_accum_ms += frame_ms;
            } else {
                state_mut.onset_accum_ms *= 1.0 - state_mut.speech_onset_decay;
            }

            let onset_fired =
                !state_mut.in_speech && state_mut.onset_accum_ms >= state_mut.speech_onset_ms;

            let frame_len = frame_slice.len() as u32;
            tracing::trace!(
                target: "ai_agent_seat::io",
                "VAD {uuid}: score={score:.3} rms={rms:.4} gate={rms_gate} \
                 thr={threshold:.2} onset={:.1}ms/{:.0}ms voiced={is_voiced}",
                state_mut.onset_accum_ms, state_mut.speech_onset_ms
            );

            if is_voiced || onset_fired {
                if !state_mut.in_speech && onset_fired {
                    state_mut.in_speech = true;
                    let pre_roll: Vec<i16> = state_mut.pre_roll.drain(..).collect();
                    let pre_roll_len = pre_roll.len();
                    state_mut.speech_buffer.clear();
                    state_mut.speech_buffer.extend(pre_roll);
                    state_mut.current_silence = 0;
                    state_mut.barge_in_confirm = 0;
                    tracing::info!(
                        "VAD: speech STARTED for {uuid} (score={score:.3} rms={rms:.4} \
                         onset={:.1}ms) pre_roll={pre_roll_len} buf={}",
                        state_mut.onset_accum_ms,
                        state_mut.speech_buffer.len()
                    );
                }
                if state_mut.in_speech {
                    state_mut.current_silence = 0;
                }
                state_mut.barge_in_confirm = 0;
            } else if state_mut.in_speech {
                state_mut.current_silence += frame_len;
                if state_mut.current_silence >= state_mut.silence_samples {
                    state_mut.in_speech = false;
                    state_mut.onset_accum_ms = 0.0;
                    let speech = std::mem::take(&mut state_mut.speech_buffer);
                    if !speech.is_empty() {
                        tracing::info!(
                            "VAD {uuid}: sending speech segment ({} samples = {:.1}s)",
                            speech.len(),
                            speech.len() as f32 / PIPELINE_SAMPLE_RATE as f32
                        );
                        match state_mut.speech_tx.as_ref() {
                            Some(tx) => {
                                if let Err(e) = tx.try_send(speech) {
                                    tracing::warn!("try_send(speech) for {uuid} failed: {e}");
                                }
                            }
                            None => {
                                tracing::warn!("speech segment for {uuid} dropped: no speech_tx");
                            }
                        }
                        // shrink_to_fit: drain(..).collect() and the silence
                        // stretch can transiently grow pre_roll's capacity
                        // above pre_roll_max. Reclaim it so a momentary frame
                        // burst doesn't permanently inflate each call's memory
                        // (×1000 concurrent calls = non-trivial RSS).
                        state_mut.pre_roll.clear();
                        state_mut.pre_roll.shrink_to_fit();
                        tracing::info!("VAD: speech ENDED for {uuid} — segment sent to actor");
                    }
                }
            }
        }

        SUCCESS
    }

    /// `read_frame`: FreeSWITCH reads audio FROM this endpoint.
    ///
    /// Drains [`CallState::tts_cons`] into the frame; fills any remainder
    /// with silence (zeros) when no TTS is available.
    fn read_frame(session: &Session, frame: &mut FrameMut) -> Status {
        let Some(uuid) = session.channel().and_then(|c| c.uuid()) else {
            return SUCCESS;
        };

        // `pcm_i16_output` sizes the slice from the codec's expected `samples`
        // and the buffer's `buflen` (NOT `datalen`, which is 0 on a fresh
        // frame), and sets `datalen` to the byte length it produced. We just
        // fill it. Without this, FreeSWITCH sees an empty frame, treats the
        // read as a break, and tears the bridge down ("ending bridge by
        // request from read function").
        let Some(buf) = frame.pcm_i16_output() else {
            return SUCCESS;
        };

        let Some(mut state) = CALLS.get_mut(&uuid) else {
            // No state yet — emit silence.
            for s in buf {
                *s = 0;
            }
            return SUCCESS;
        };
        let state_mut = state.value_mut();

        // Drain the SPSC ringbuf consumer into `buf`; zero-fill the rest.
        // `pop_slice` is the single consumer's read — the FS media thread owns
        // this consumer exclusively (the producer lives in the TTS driver loop
        // on the tokio runtime), so the DashMap `RefMut` borrow is sufficient.
        let written = match state_mut.tts_cons.as_mut() {
            Some(cons) => cons.pop_slice(buf),
            None => 0,
        };
        for s in &mut buf[written..] {
            *s = 0;
        }

        SUCCESS
    }

    /// `kill_channel`: call ended — remove the [`CallState`] from [`CALLS`].
    fn kill_channel(session: &Session, sig: i32) -> Status {
        // FreeSWITCH signals: NONE=0, KILL=1, XFER=2, BREAK=3. Only KILL ends the
        // call; BREAK/XFER are media-control signals sent during bridge setup and
        // mid-call (e.g. on barge-in). Cleaning up CallState on a non-KILL signal
        // would orphan the call — `write_frame` would then re-init a fresh
        // CallState (losing orchestrator/TTS) or fail to find one, breaking VAD.
        const SIG_KILL: i32 = 1;
        if sig != SIG_KILL {
            tracing::trace!("kill_channel sig={sig} (non-KILL; keeping call state)");
            return SUCCESS;
        }

        let Some(uuid) = session.channel().and_then(|c| c.uuid()) else {
            return SUCCESS;
        };

        if let Some((_, mut state)) = CALLS.remove(&uuid) {
            // Kill the CallActor: immediately aborts its task (any in-flight
            // turn) and drops its state (tts_session → WS Shutdown). This is
            // the strong lifecycle boundary that fixes the old "TTS keeps
            // running after hangup" leak.
            if let Some(actor_ref) = state.actor.take() {
                actor_ref.kill();
            }
            tracing::info!("kill_channel: removed call state for {uuid}");
        }
        crate::call_core::unregister_call(&uuid);
        SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use ringbuf::traits::{Consumer, Observer, Producer};
    use std::sync::Arc;
    use crate::io::rms;

    /// The ringbuf capacity used in production (from actor::init_call).
    const RINGBUF_CAPACITY: usize = 160000;

    /// Simulate the TTS → ringbuf → read_frame pipeline.
    /// Returns (samples_pushed, samples_popped, samples_dropped).
    fn simulate_tts_pipeline(
        total_audio_samples: usize,
        chunk_size: usize,
        pop_rate_per_sec: usize,
        duration_ms: u64,
    ) -> (usize, usize, usize) {
        let rb = Arc::new(ringbuf::HeapRb::<i16>::new(RINGBUF_CAPACITY));
        let mut prod = ringbuf::Prod::new(rb.clone());
        let mut cons = ringbuf::Cons::new(rb);

        let audio: Vec<i16> = (0..total_audio_samples)
            .map(|i| (i % 1000) as i16)
            .collect();

        let mut pushed = 0;
        let mut popped = 0;
        let mut offset = 0;

        let pop_interval_ms = 20u64;
        let pop_samples = pop_rate_per_sec * pop_interval_ms as usize / 1000;
        let mut elapsed = 0u64;

        while elapsed < duration_ms {
            // Producer: push one chunk (simulates TTS arriving in bursts)
            if offset < audio.len() {
                let end = (offset + chunk_size).min(audio.len());
                let chunk = &audio[offset..end];
                let n = prod.push_slice(chunk);
                pushed += n;
                offset += chunk.len();
            }

            // Consumer: pop at fixed rate (simulates read_frame every 20ms)
            let mut buf = vec![0i16; pop_samples];
            let n = cons.pop_slice(&mut buf);
            popped += n;

            elapsed += pop_interval_ms;
        }

        let dropped = total_audio_samples.min(offset) - pushed;
        (pushed, popped, dropped)
    }

    #[test]
    fn ringbuf_overflow_long_sentence() {
        // 57-char Chinese sentence ≈ 5s at 8kHz = 40000 samples.
        // Ringbuf capacity = 32000 (4s). Expect overflow → dropped samples.
        let total_samples = 40000;
        let chunk_size = 5120; // typical TTS chunk
        let pop_rate = 8000;
        let duration = 6000;

        let (pushed, popped, dropped) =
            simulate_tts_pipeline(total_samples, chunk_size, pop_rate, duration);

        eprintln!(
            "Long sentence: pushed={pushed} popped={popped} dropped={dropped} / total={total_samples}"
        );
        assert_eq!(
            dropped, 0,
            "BUG: {dropped} samples dropped! push_slice silently discards on overflow. \
             Capacity={RINGBUF_CAPACITY}=4s, sentence=5s. \
             FIX: increase to 80000+ (10s)."
        );
    }

    #[test]
    fn ringbuf_ok_short_sentence() {
        // 12-char sentence ≈ 2s at 8kHz = 16000 samples. Fits in 32000.
        let total_samples = 16000;
        let (pushed, popped, dropped) = simulate_tts_pipeline(total_samples, 5120, 8000, 3000);

        eprintln!("Short: pushed={pushed} popped={popped} dropped={dropped}");
        assert_eq!(dropped, 0);
        assert_eq!(pushed, total_samples);
        assert!(popped >= total_samples - 320);
    }

    #[test]
    fn ringbuf_clear_prevents_turn_overlap() {
        let rb = Arc::new(ringbuf::HeapRb::<i16>::new(RINGBUF_CAPACITY));
        let mut prod = ringbuf::Prod::new(rb.clone());
        let mut cons = ringbuf::Cons::new(rb);

        // Turn 1: push 8000 samples, consume only 4000
        let turn1: Vec<i16> = (0..8000).map(|i| (i % 100) as i16).collect();
        assert_eq!(prod.push_slice(&turn1), 8000);
        let mut buf = vec![0i16; 4000];
        assert_eq!(cons.pop_slice(&mut buf), 4000);
        assert_eq!(cons.occupied_len(), 4000);

        // Turn 2 starts: clear ringbuf
        cons.clear();
        assert_eq!(cons.occupied_len(), 0, "Clear must remove leftover");

        // Turn 2: push new audio
        let turn2: Vec<i16> = (0..4000).map(|i| (500 + i % 100) as i16).collect();
        assert_eq!(prod.push_slice(&turn2), 4000);

        let mut buf2 = vec![0i16; 4000];
        assert_eq!(cons.pop_slice(&mut buf2), 4000);
        assert_eq!(buf2[0], 500, "Must be turn 2 data, not turn 1");
    }

    #[test]
    fn ringbuf_capacity_sufficient_for_10s() {
        let seconds = RINGBUF_CAPACITY as f64 / 8000.0;
        eprintln!("Ringbuf: {RINGBUF_CAPACITY} samples = {seconds}s at 8kHz");
        assert!(
            seconds >= 10.0,
            "BUG: Ringbuf holds only {seconds}s. Long TTS (5-10s) overflows. \
             FIX: increase to 80000 (10s)."
        );
    }

    /// Regression: pre_roll must be trimmed to a FIXED size, NOT
    /// `VecDeque::capacity()` (which grows on reallocation, turning pre_roll
    /// into an unbounded buffer that swallowed 11.8s of audio).
    #[test]
    fn pre_roll_trimmed_to_fixed_size() {
        use std::collections::VecDeque;
        const PRE_ROLL_MAX: usize = 2400; // 300ms at 8kHz

        // Simulate: pre_roll receives 60s of audio (way beyond capacity).
        // With the old `capacity()`-based trim, it would grow unboundedly.
        let mut pre_roll: VecDeque<i16> = VecDeque::with_capacity(PRE_ROLL_MAX);
        // Push 60 seconds = 480000 samples in 160-sample chunks (20ms frames)
        for _ in 0..3000 {
            pre_roll.extend(std::iter::repeat(0i16).take(160));
            // OLD (buggy): while pre_roll.len() > pre_roll.capacity() { ... }
            // NEW (fixed): trim to the fixed constant
            while pre_roll.len() > PRE_ROLL_MAX {
                pre_roll.pop_front();
            }
        }

        assert_eq!(
            pre_roll.len(),
            PRE_ROLL_MAX,
            "BUG: pre_roll grew to {} (capacity={}), expected max {PRE_ROLL_MAX}. \
             Trimming to `capacity()` is wrong — VecDeque reallocates and grows.",
            pre_roll.len(),
            pre_roll.capacity()
        );
        eprintln!(
            "pre_roll: len={} capacity={}",
            pre_roll.len(),
            pre_roll.capacity()
        );
    }

    /// Benchmark earshot VAD predict + RMS throughput on a single core.
    /// Run: `cargo test -p ai-agent-seat --release -- io::tests::bench_vad_throughput --nocapture --ignored`
    /// Answers the capacity question: how many concurrent VAD calls can one
    /// core sustain at 50 Hz (20 ms frames)?
    #[test]
    #[ignore]
    fn bench_vad_throughput() {
        let mut detector = earshot::Detector::default();
        // Synthetic 256-sample frame with moderate energy (not silence, so the
        // RMS gate doesn't skip — measures the full predict path).
        let frame: [i16; 256] = (0..256)
            .map(|i| ((i as f32 * 0.5).sin() * 8000.0) as i16)
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();

        // Warmup
        for _ in 0..1000 {
            detector.predict_i16(&frame);
        }

        const N: usize = 100_000;
        let t = std::time::Instant::now();
        let mut acc = 0.0f32;
        for _ in 0..N {
            acc += detector.predict_i16(&frame);
        }
        let predict_ns = t.elapsed().as_nanos() as f64 / N as f64;

        let t = std::time::Instant::now();
        for _ in 0..N {
            std::hint::black_box(rms(&frame));
        }
        let rms_ns = t.elapsed().as_nanos() as f64 / N as f64;

        let predicts_per_sec = 1e9 / predict_ns;
        // At 50 Hz per call, one core handles this many full-predict calls:
        let calls_per_core_full = predicts_per_sec / 50.0;
        // Silent calls only pay RMS:
        let silent_calls_per_core = (1e9 / rms_ns) / 50.0;

        eprintln!("=== VAD benchmark (single core, release) ===");
        eprintln!("predict_i16: {predict_ns:.0} ns/call  → {predicts_per_sec:.0} predicts/sec");
        eprintln!("rms (integer): {rms_ns:.0} ns/call");
        eprintln!("capacity @50Hz/call:");
        eprintln!("  ALL calls talking (full predict): {calls_per_core_full:.0} calls/core");
        eprintln!("  silent calls (RMS-gated, predict skipped): {silent_calls_per_core:.0} calls/core");
        eprintln!("  [acc={acc}]"); // prevent dead-code elim
    }
}
