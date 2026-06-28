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
//!   We drain [`CallState::tts_accum`] into the frame; silence when empty.
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

use dashmap::DashMap;
use fswtch::{
    CallerProfile, EndpointIoRoutines, EndpointInterfaceRef, Frame, FrameMut, OriginateFlag,
    OutgoingResult, Session, Status, SUCCESS, request_session,
};

use crate::audio_dsp::{PIPELINE_SAMPLE_RATE, SampleRateConverter, get_codec_rate};
use crate::voice_core::Config;

/// earshot predicts on fixed 256-sample (16 ms at 16 kHz) frames.
const EARSHOT_FRAME_SIZE: usize = 256;

/// Score above which an earshot frame is considered speech.
const SPEECH_SCORE_THRESHOLD: f32 = 0.5;

/// Per-call state, owned by the global [`CALLS`] map and borrowed by the I/O
/// callbacks + the orchestrator's spawned tasks.
///
/// `tts_accum` is the bridge from the orchestrator (async, tokio runtime) to
/// `read_frame` (sync, FS media thread): the orchestrator pushes 16 kHz i16
/// PCM here, `read_frame` drains it. A [`DashMap`] entry's `RefMut` gives us
/// the exclusive borrow `read_frame` needs without a separate lock.
///
/// `ai_speaking` is shared by value (Arc-clone) with the orchestrator so it
/// can set/clear it without touching the map.
pub struct CallState {
    pub uuid: String,
    /// earshot VAD detector (16 kHz). `None` when the pipeline rate is not 16 kHz.
    pub vad: Option<earshot::Detector>,
    /// Upsampler from the session's codec rate to 16 kHz, when they differ.
    pub resampler: Option<SampleRateConverter>,
    /// Pre-roll buffer (300 ms at 16 kHz) captured before speech onset.
    pub pre_roll: VecDeque<i16>,
    /// Whether we are currently inside a speech segment.
    pub in_speech: bool,
    /// Accumulated 16 kHz i16 PCM for the current speech segment.
    pub speech_buffer: Vec<i16>,
    /// Staging buffer for accumulating samples into earshot-sized (256) frames.
    pub vad_stage: Vec<i16>,
    /// Silence-timeout in samples (16 kHz). When `current_silence` exceeds
    /// this, the speech segment is considered complete.
    pub silence_samples: u32,
    /// Current silence sample count within a speech segment.
    pub current_silence: u32,
    /// Barge-in confirmation threshold in samples.
    pub barge_in_confirm_samples: u32,
    /// Current barge-in confirmation counter.
    pub barge_in_confirm: u32,
    /// TTS audio to play toward the caller (16 kHz i16 PCM). Drained by
    /// `read_frame`; filled by the orchestrator's `synthesize_and_play`.
    pub tts_accum: VecDeque<i16>,
    /// AI-speaking flag, shared (Arc-clone) with the orchestrator.
    pub ai_speaking: Arc<AtomicBool>,
    /// Loaded configuration snapshot (may be `None` → defaults).
    pub config: Option<Config>,
    /// The orchestrator owning this call's AI pipeline. `None` until
    /// [`actor::init_call`] constructs it.
    pub orchestrator: Option<Arc<crate::orchestrator::Orchestrator>>,
}

impl CallState {
    /// Build a fresh `CallState` for `uuid` with the given codec rate and config.
    ///
    /// `codec_rate` is the session's read-codec sample rate (Hz); the resampler
    /// is created when it differs from [`PIPELINE_SAMPLE_RATE`].
    pub fn new(uuid: String, codec_rate: u32, config: Option<Config>) -> anyhow::Result<Self> {
        let resampler = if codec_rate != PIPELINE_SAMPLE_RATE {
            Some(SampleRateConverter::new(codec_rate, PIPELINE_SAMPLE_RATE)?)
        } else {
            None
        };

        let vad = if PIPELINE_SAMPLE_RATE == 16000 {
            Some(earshot::Detector::default())
        } else {
            None
        };

        let pre_roll_size = (PIPELINE_SAMPLE_RATE * 300 / 1000) as usize;

        let vad_config = config.as_ref().map(|c| c.vad.clone()).unwrap_or_default();
        let silence_samples = vad_config.silence_timeout_ms * PIPELINE_SAMPLE_RATE / 1000;
        let barge_in_confirm_samples = vad_config.barge_in_confirm_ms * PIPELINE_SAMPLE_RATE / 1000;

        Ok(Self {
            uuid,
            vad,
            resampler,
            pre_roll: VecDeque::with_capacity(pre_roll_size),
            in_speech: false,
            speech_buffer: Vec::new(),
            vad_stage: Vec::with_capacity(EARSHOT_FRAME_SIZE),
            silence_samples,
            current_silence: 0,
            barge_in_confirm_samples,
            barge_in_confirm: 0,
            tts_accum: VecDeque::new(),
            ai_speaking: Arc::new(AtomicBool::new(false)),
            config,
            orchestrator: None,
        })
    }

    /// Push 16 kHz i16 PCM into the TTS accumulator (called from the
    /// orchestrator's async task).
    pub fn push_tts(&mut self, samples: &[i16]) {
        self.tts_accum.extend(samples.iter().copied());
    }

    /// Drop any buffered TTS audio (barge-in flush).
    pub fn clear_tts(&mut self) {
        self.tts_accum.clear();
    }
}

/// Global registry: call UUID → per-call state.
///
/// Inserted lazily by `write_frame` (via [`actor::init_call`]) on first frame,
/// removed by `kill_channel`. The DashMap shards across cores so the 50 Hz
/// media-thread lookups don't contend.
pub static CALLS: std::sync::LazyLock<DashMap<String, CallState>> =
    std::sync::LazyLock::new(DashMap::new);

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
        let Some(new_session) =
            request_session(endpoint, fswtch::CallDirection::OUTBOUND, flags)
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

        // Initialize read + write codecs (L16 at the loopback's 8 kHz, 20 ms).
        // Without a read codec, `switch_core_io` hangs the channel up with
        // `SWITCH_CAUSE_INCOMPATIBLE_DESTINATION` the instant media exchange
        // begins. L16 (linear PCM) needs no transcoding against the loopback
        // A leg and lets our `read_frame`/`write_frame` see raw i16 samples.
        let codec_rate = get_codec_rate(&new_session).max(8000);
        if let Err(e) = new_session.init_read_codec("L16", codec_rate, 20, 1) {
            tracing::warn!("outgoing_channel: init_read_codec failed: {e}");
        }
        if let Err(e) = new_session.init_write_codec("L16", codec_rate, 20, 1) {
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

        let Some(samples) = frame.pcm_i16() else {
            return SUCCESS;
        };
        if samples.is_empty() {
            return SUCCESS;
        }

        // Upsample to 16 kHz if the codec rate differs.
        let samples_16k: Vec<i16> = if let Some(resampler) = state_mut.resampler.as_mut() {
            resampler.process(samples)
        } else {
            samples.to_vec()
        };

        // VAD: accumulate into 256-sample frames and score each.
        state_mut.vad_stage.extend_from_slice(&samples_16k);
        while state_mut.vad_stage.len() >= EARSHOT_FRAME_SIZE {
            let frame_slice: [i16; EARSHOT_FRAME_SIZE] = {
                let mut buf = [0i16; EARSHOT_FRAME_SIZE];
                buf.copy_from_slice(&state_mut.vad_stage[..EARSHOT_FRAME_SIZE]);
                buf
            };
            state_mut.vad_stage.drain(..EARSHOT_FRAME_SIZE);

            let score = match state_mut.vad.as_mut() {
                Some(detector) => detector.predict_i16(&frame_slice),
                None => -1.0,
            };
            let is_speech = score >= SPEECH_SCORE_THRESHOLD;
            let frame_len = frame_slice.len() as u32;

            if is_speech {
                if !state_mut.in_speech {
                    state_mut.in_speech = true;
                    state_mut.speech_buffer.clear();
                    state_mut.speech_buffer.extend(state_mut.pre_roll.iter());
                    state_mut.current_silence = 0;
                    tracing::debug!("Speech started for {uuid}");
                }
                state_mut.speech_buffer.extend_from_slice(&frame_slice);
                state_mut.barge_in_confirm = 0;
            } else if state_mut.in_speech {
                state_mut.current_silence += frame_len;
                if state_mut.current_silence >= state_mut.silence_samples {
                    state_mut.in_speech = false;
                    let speech = std::mem::take(&mut state_mut.speech_buffer);
                    if !speech.is_empty() {
                        let orch = state_mut.orchestrator.clone();
                        let uuid_for_task = uuid.clone();
                        crate::runtime::spawn(async move {
                            if let Some(orch) = orch {
                                match orch.process_speech_segment(speech).await {
                                    Some((reply, asr)) => {
                                        tracing::info!(
                                            "turn complete for {uuid_for_task}: \
                                             reply={} chars, asr={:?}",
                                            reply.chars().count(),
                                            asr
                                        );
                                    }
                                    None => {
                                        tracing::info!("turn discarded for {uuid_for_task}");
                                    }
                                }
                            }
                        });
                        state_mut.pre_roll.clear();
                        tracing::debug!("Speech ended for {uuid}");
                    }
                } else {
                    state_mut.speech_buffer.extend_from_slice(&frame_slice);
                }
            } else {
                state_mut.pre_roll.extend(&frame_slice);
                while state_mut.pre_roll.len() > state_mut.pre_roll.capacity() {
                    state_mut.pre_roll.pop_front();
                }
            }
        }

        SUCCESS
    }

    /// `read_frame`: FreeSWITCH reads audio FROM this endpoint.
    ///
    /// Drains [`CallState::tts_accum`] into the frame; fills any remainder
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

        // Drain TTS accumulator into `buf`; zero-fill the rest.
        let mut written = 0usize;
        while written < buf.len() {
            let Some(sample) = state_mut.tts_accum.pop_front() else {
                break;
            };
            buf[written] = sample;
            written += 1;
        }
        for s in &mut buf[written..] {
            *s = 0;
        }

        SUCCESS
    }

    /// `kill_channel`: call ended — remove the [`CallState`] from [`CALLS`].
    fn kill_channel(session: &Session, _sig: i32) -> Status {
        let Some(uuid) = session.channel().and_then(|c| c.uuid()) else {
            return SUCCESS;
        };

        if let Some((_, mut state)) = CALLS.remove(&uuid) {
            if let Some(orch) = state.orchestrator.take() {
                orch.full_hangup_reset();
            }
            tracing::info!("kill_channel: removed call state for {uuid}");
        }
        crate::call_core::unregister_call(&uuid);
        SUCCESS
    }
}

