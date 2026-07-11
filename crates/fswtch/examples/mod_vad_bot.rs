//! mod_vad_bot — endpoint-based VAD PCM bridge (the bot IS the call terminus).
//!
//! Companion to mod_vad_pcm (the media-bug variant): the **event contract is identical**
//! (outbound `fswtch::asr_result` per talking frame, body=base64 PCM + audio headers; inbound
//! `fswtch::play_pcm` with base64 PCM + `Target-UUID`), but the **media primitive is a custom
//! FreeSWITCH endpoint**, not a media bug. The caller bridges to `fswtch_vad_bot/<num>`;
//! FreeSWITCH drives the call's media through this module's [`fswtch::EndpointIoRoutines`]:
//!
//! - `write_frame` — FreeSWITCH writes the CALLER'S audio TO this endpoint. VAD runs here; on a
//!   talking-active frame a `fswtch::asr_result` event fires with the frame's base64 PCM.
//! - `read_frame` — FreeSWITCH reads audio FROM this endpoint (toward the caller). We drain the
//!   per-call TTS queue (fed by `fswtch::play_pcm`) into the frame; silence when empty. The full
//!   frame is ALWAYS filled — an empty frame makes FreeSWITCH treat the read as a break and tear
//!   the bridge down.
//! - `kill_channel` (`SIG_KILL` only) — drop the per-call state.
//! - `outgoing_channel` — create the B leg when the dialplan bridges to `fswtch_vad_bot/<num>`.
//!
//! # Why endpoint vs media bug
//! - mod_vad_pcm (bug) taps/overrides an **existing** call's media leg (barge-in / monitoring on
//!   a human-human call, or a call you didn't set up). Idle audio passes through.
//! - mod_vad_bot (endpoint) **is** the call's media terminus: the bot is the party the caller
//!   talks to. No `WRITE_REPLACE` hack — `read_frame`/`write_frame` are the native media path.
//!   Idle audio is silence (there is no "other party" to pass through).
//!
//! # Use
//! ```text
//! load mod_vad_bot
//! # dialplan: bridge the caller to the bot endpoint
//! <action application="bridge" data="fswtch_vad_bot/1000"/>
//! # downstream brain: subscribe `event custom fswtch::asr_result`, base64-decode each body to
//! # S16LE PCM at the advertised Sample-Rate/Channels, run ASR, then TTS and send each chunk
//! # back as a fswtch::play_pcm event (same headers + Target-UUID + base64 PCM body).
//! # on barge-in (caller interrupts the bot): flush the play buffer to stop TTS at once:
//! fs_cli -x 'fswtch_vad_bot_stop_playback <uuid>'
//! ```
//! Load mod_vad_pcm OR mod_vad_bot, not both — both subscribe to `fswtch::play_pcm`.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, LazyLock, Mutex};

use base64::{Engine, engine::general_purpose::STANDARD};
use fswtch::{
    CallDirection, CallerProfile, ChannelState, EndpointInterfaceRef, EndpointIoBuilder,
    EndpointIoRoutines, Frame, FrameMut, OriginateFlag, OutgoingResult, SUCCESS, Session,
    StateHandlerTable, Status, Vad, VadState, request_session,
};

const ASR_SUBCLASS: &str = "fswtch::asr_result";
const PLAY_SUBCLASS: &str = "fswtch::play_pcm";
/// Play-queue high-water mark in seconds of audio — further `play_pcm` events are dropped.
const PLAY_QUEUE_MAX_SECS: u32 = 10;
/// Pipeline sample rate / packetization this endpoint speaks (L16, 20 ms, mono).
const PIPELINE_RATE: u32 = 8000;
const FRAME_MS: u32 = 20;
const CHANNELS: u32 = 1;
/// FreeSWITCH `switch_signal_t` values: NONE=0, KILL=1, XFER=2, BREAK=3. Only KILL ends the call.
const SIG_KILL: i32 = 1;

fswtch::module_exports! {
    module = mod_vad_bot,
    load = switch_module_load,
}

// ── per-call state ──────────────────────────────────────────────────────────
//
// The I/O callbacks receive no `user_data`, so per-call state is recovered by session UUID via
// the global [`REGISTRY`]. The map lock is held only to grab the per-call `Arc`; the heavy
// per-frame work (VAD, base64, event fire, queue drain) runs under the per-call lock so calls
// don't serialize against each other.

struct CallState {
    vad: Vad,
    /// TTS samples (pipeline rate, mono) drained by `read_frame`; filled by `on_play_pcm`.
    tts_queue: VecDeque<i16>,
    sample_rate: u32,
    channels: u32,
    /// Outbound sequence number (per call).
    seq: u64,
    /// Scratch for the mono downmix fed to `Vad::process` (reused per frame; no per-frame alloc).
    mono_scratch: Vec<i16>,
}

// SAFETY: `CallState`'s only `!Send` field is `vad: Vad`. `Vad` carries a `PhantomData<*const ()>`
// (its `process(&self)` mutates internal state, marking it `!Sync`) and a `NonNull` to an OWNED
// `switch_vad_t` (allocated in `Vad::new`, freed in `Drop`). Moving it between threads is sound:
// the `switch_vad_t` is exclusively owned and every access happens under the per-call `Mutex`.
// All other fields (`VecDeque`, `Vec`, `u32`, `u64`, `String`) are `Send`, so `CallState: Send`.
unsafe impl Send for CallState {}

static REGISTRY: LazyLock<Mutex<HashMap<String, Arc<Mutex<CallState>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Look up a call's state, dropping the registry lock before the caller touches the per-call lock.
fn lookup(uuid: &str) -> Option<Arc<Mutex<CallState>>> {
    REGISTRY.lock().ok()?.get(uuid).cloned()
}

// ── outbound: fire one fswtch::asr_result per talking-active frame ───────────

#[allow(clippy::too_many_arguments)] // eight header fields map 1:1 onto add_header calls
fn fire_asr(
    uuid: &str,
    vad_state: &str,
    seq: u64,
    sample_rate: u32,
    channels: u32,
    samples: u32,
    body: &str,
) -> fswtch::Result<()> {
    let mut ev = fswtch::Event::custom(ASR_SUBCLASS)?;
    ev.add_header("Call-UUID", uuid)?;
    ev.add_header("Vad-State", vad_state)?;
    ev.add_header("Seq", &seq.to_string())?;
    ev.add_header("Sample-Rate", &sample_rate.to_string())?;
    ev.add_header("Channels", &channels.to_string())?;
    ev.add_header("Bits-Per-Sample", "16")?;
    ev.add_header("Sample-Format", "S16LE")?;
    ev.add_header("Samples", &samples.to_string())?;
    ev.add_body(body)?;
    ev.fire()
}

// ── the endpoint ────────────────────────────────────────────────────────────

pub struct VadBot;

impl EndpointIoRoutines for VadBot {
    const NAME: &'static str = "fswtch_vad_bot";

    /// Create the B leg when FreeSWITCH bridges to `fswtch_vad_bot/<num>`.
    ///
    /// Requests a session on this endpoint, installs the caller profile + name, marks the channel
    /// answered (so the originator's bridge completes), flags audio media (so FS wires our
    /// `read_frame`/`write_frame` into the bridge), initializes the L16 codecs, drives the state
    /// machine to `CS_CONSUME_MEDIA`, and registers the per-call [`CallState`]. We do NOT call
    /// `thread_launch` — `switch_ivr_originate` does that after we return `CAUSE_SUCCESS`.
    fn outgoing_channel(
        _session: Option<&Session>,
        caller_profile: Option<CallerProfile>,
        endpoint: &EndpointInterfaceRef,
        flags: OriginateFlag,
    ) -> OutgoingResult {
        let Some(new_session) = request_session(endpoint, CallDirection::OUTBOUND, flags) else {
            fswtch::log_error("mod_vad_bot", "outgoing_channel: request_session failed");
            return OutgoingResult::refused();
        };
        let Some(channel) = new_session.channel() else {
            fswtch::log_error("mod_vad_bot", "outgoing_channel: no channel");
            new_session.hangup(fswtch::Cause::DESTINATION_OUT_OF_ORDER);
            return OutgoingResult::refused();
        };

        if let Some(ref profile) = caller_profile {
            channel.set_caller_profile(profile);
        }
        let _ = channel.set_name("fswtch_vad_bot");
        let _ = channel.mark_answered();
        channel.set_audio_flag();
        if let Err(error) = new_session.init_read_codec("L16", PIPELINE_RATE, FRAME_MS, CHANNELS) {
            fswtch::log_error("mod_vad_bot", format!("init_read_codec failed: {error}"));
        }
        if let Err(error) = new_session.init_write_codec("L16", PIPELINE_RATE, FRAME_MS, CHANNELS) {
            fswtch::log_error("mod_vad_bot", format!("init_write_codec failed: {error}"));
        }
        // Drive the leg out of CS_NEW. CS_CONSUME_MEDIA is the terminal "wait for media" state;
        // our all-NULL state-handler table lets the standard on_consume_media handler run (a
        // no-op), then the thread sleeps on its condvar until hangup. The A leg's bridge then
        // drives read/write frames through our I/O routines.
        channel.set_state(ChannelState::CONSUME_MEDIA);

        let uuid = channel.uuid().unwrap_or_default();
        if uuid.is_empty() {
            fswtch::log_error("mod_vad_bot", "outgoing_channel: channel has no uuid");
            new_session.hangup(fswtch::Cause::DESTINATION_OUT_OF_ORDER);
            return OutgoingResult::refused();
        }
        let vad = match Vad::new(PIPELINE_RATE as i32, CHANNELS as i32) {
            Ok(vad) => vad,
            Err(error) => {
                fswtch::log_error("mod_vad_bot", format!("vad init failed: {error}"));
                new_session.hangup(fswtch::Cause::DESTINATION_OUT_OF_ORDER);
                return OutgoingResult::refused();
            }
        };
        let state = Arc::new(Mutex::new(CallState {
            vad,
            tts_queue: VecDeque::new(),
            sample_rate: PIPELINE_RATE,
            channels: CHANNELS,
            seq: 0,
            mono_scratch: Vec::new(),
        }));
        if let Ok(mut reg) = REGISTRY.lock() {
            reg.insert(uuid.clone(), state);
            fswtch::log_info("mod_vad_bot", format!("call registered: {uuid}"));
        }
        fswtch::log_info(
            "mod_vad_bot",
            format!("outgoing_channel: created session {uuid}"),
        );
        OutgoingResult::success(new_session)
    }

    /// FreeSWITCH writes the CALLER'S audio TO this endpoint. VAD runs here on a mono downmix of
    /// the frame; on a talking-active frame we fire `fswtch::asr_result` whose body is the frame's
    /// ORIGINAL interleaved PCM (base64) — not the downmix — with the real `Channels` header. The
    /// frame is read-only.
    fn write_frame(session: &Session, frame: &Frame) -> Status {
        let Some(uuid) = session.channel().and_then(|c| c.uuid()) else {
            return SUCCESS;
        };
        let Some(pcm) = frame.pcm_i16() else {
            return SUCCESS; // non-16-bit-linear frame: nothing to VAD
        };
        let channels = frame.channels().max(1) as usize;
        let Some(state) = lookup(&uuid) else {
            return SUCCESS; // no state (outgoing_channel didn't register) — nothing to VAD
        };

        // Per-call lock: mono downmix + VAD + snapshot the event fields, then drop the lock.
        let (label, seq) = {
            let mut guard = match state.lock() {
                Ok(g) => g,
                Err(_) => return SUCCESS, // poisoned: skip this frame
            };
            // Deref to a direct `&mut CallState` so disjoint field borrows (`vad` shared,
            // `mono_scratch` mut) work — through `MutexGuard`'s Deref/DerefMut the checker can't
            // split them.
            let s = &mut *guard;
            s.mono_scratch.clear();
            if channels == 1 {
                s.mono_scratch.extend_from_slice(pcm);
            } else {
                s.mono_scratch.reserve(pcm.len() / channels);
                for chunk in pcm.chunks_exact(channels) {
                    let sum: i64 = chunk.iter().map(|&x| x as i64).sum();
                    s.mono_scratch.push((sum / channels as i64) as i16);
                }
            }
            let state_label = match s.vad.process(&mut s.mono_scratch) {
                VadState::START_TALKING => "start-talking",
                VadState::TALKING => "talking",
                VadState::STOP_TALKING => "stop-talking",
                _ => return SUCCESS, // NONE / ERROR: silence is not forwarded
            };
            s.seq = s.seq.wrapping_add(1);
            (state_label, s.seq)
        };

        // Lock released: base64-encode + fire outside the per-call lock.
        let body = STANDARD.encode(frame.bytes());
        if let Err(error) = fire_asr(
            &uuid,
            label,
            seq,
            frame.rate(),
            frame.channels(),
            frame.samples(),
            &body,
        ) {
            fswtch::log_error("mod_vad_bot", format!("fire asr_result failed: {error}"));
        }
        SUCCESS
    }

    /// FreeSWITCH reads audio FROM this endpoint (toward the caller). Drain the TTS queue into the
    /// frame; silence when empty. The frame is ALWAYS filled — an empty frame (datalen 0) makes
    /// FreeSWITCH treat the read as a break and tear the bridge down ("ending bridge by request
    /// from read function").
    fn read_frame(session: &Session, frame: &mut FrameMut) -> Status {
        // `pcm_i16_output` sizes the slice from the codec's expected `samples` and the buffer's
        // `buflen` (datalen is 0 on a fresh frame) and sets datalen to the byte length produced.
        let Some(buf) = frame.pcm_i16_output() else {
            return SUCCESS; // can't size output (rare; codec is L16) — leave it to FS
        };
        let Some(uuid) = session.channel().and_then(|c| c.uuid()) else {
            buf.fill(0);
            return SUCCESS;
        };
        let Some(state) = lookup(&uuid) else {
            buf.fill(0);
            return SUCCESS;
        };
        match state.lock() {
            Ok(mut s) => {
                let q = &mut s.tts_queue;
                for slot in buf.iter_mut() {
                    *slot = q.pop_front().unwrap_or(0); // underrun → silence
                }
            }
            Err(_) => buf.fill(0), // poisoned → silence
        }
        SUCCESS
    }

    /// Hangup signal. Only `SIG_KILL` ends the call — `BREAK`/`XFER` are mid-call media-control
    /// signals sent during bridge setup / barge-in; cleaning up on those would orphan the call.
    fn kill_channel(session: &Session, sig: i32) -> Status {
        if sig != SIG_KILL {
            return SUCCESS;
        }
        let Some(uuid) = session.channel().and_then(|c| c.uuid()) else {
            return SUCCESS;
        };
        if let Ok(mut reg) = REGISTRY.lock()
            && reg.remove(&uuid).is_some()
        {
            fswtch::log_info("mod_vad_bot", format!("call ended: {uuid}"));
        }
        SUCCESS
    }
}

// ── inbound: fswtch::play_pcm (base64 PCM) → TTS queue ──────────────────────

fswtch::event_callback! {
    fn on_play_pcm(event) {
        let target = match event.header("Target-UUID") {
            Some(t) if !t.is_empty() => t,
            _ => {
                fswtch::log_error("mod_vad_bot", "play_pcm event missing Target-UUID");
                return;
            }
        };
        let rate_hdr = event.header("Sample-Rate").and_then(|s| s.parse::<u32>().ok());
        let chan_hdr = event.header("Channels").and_then(|s| s.parse::<u32>().ok());
        let body = event.body_str().unwrap_or_default();
        let bytes = match STANDARD.decode(body.as_bytes()) {
            Ok(b) => b,
            Err(error) => {
                fswtch::log_error(
                    "mod_vad_bot",
                    format!("play_pcm base64 decode failed: {error}"),
                );
                return;
            }
        };
        let samples: Vec<i16> = bytes
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();
        if samples.is_empty() {
            return;
        }
        let Some(state) = lookup(&target) else {
            fswtch::log_error("mod_vad_bot", format!("no active call on {target}"));
            return;
        };
        match state.lock() {
            Ok(mut s) => {
                if let Some(rate) = rate_hdr
                    && rate != s.sample_rate
                {
                    fswtch::log_error(
                        "mod_vad_bot",
                        format!(
                            "play_pcm rate mismatch on {target}: got {rate}, want {}",
                            s.sample_rate
                        ),
                    );
                    return;
                }
                if let Some(ch) = chan_hdr && ch != s.channels {
                    fswtch::log_error(
                        "mod_vad_bot",
                        format!(
                            "play_pcm channels mismatch on {target}: got {ch}, want {}",
                            s.channels
                        ),
                    );
                    return;
                }
                let max_samples = s.sample_rate.saturating_mul(PLAY_QUEUE_MAX_SECS) as usize;
                // Drop oldest so the fresh chunk fits within cap — bounds playback latency while
                // always admitting the latest TTS (rather than discarding the new event). If a
                // single chunk alone exceeds cap, keep only its cap-sized tail.
                if samples.len() >= max_samples {
                    s.tts_queue.clear();
                    s.tts_queue
                        .extend(samples[samples.len() - max_samples..].iter().copied());
                } else {
                    while s.tts_queue.len() + samples.len() > max_samples {
                        s.tts_queue.pop_front();
                    }
                    s.tts_queue.extend(samples.iter().copied());
                }
            }
            Err(_) => fswtch::log_error("mod_vad_bot", "call lock poisoned"),
        }
    }
}

// ── APIs ────────────────────────────────────────────────────────────────────

fswtch::api_callback! {
    fn stop_playback_api(cmd, _session, stream) {
        fswtch::log_info("mod_vad_bot", "fswtch_vad_bot_stop_playback invoked");
        let Some(stream) = stream else {
            return fswtch::FALSE;
        };
        let uuid = cmd.unwrap_or_default().trim().to_owned();
        if uuid.is_empty() {
            return stream.write("usage: fswtch_vad_bot_stop_playback <uuid>\n");
        }
        let Some(state) = lookup(&uuid) else {
            return stream.write(&format!("no active call on {uuid}\n"));
        };
        // Flushing the queue stops TTS within one read_frame: the next `read_frame` finds an empty
        // queue and fills the frame with silence, while `write_frame` keeps VAD'ing the caller's
        // barge-in speech.
        let cleared = match state.lock() {
            Ok(mut s) => {
                let n = s.tts_queue.len();
                s.tts_queue.clear();
                n
            }
            Err(_) => return stream.write("call lock poisoned\n"),
        };
        stream.write(&format!("cleared {cleared} queued samples on {uuid}\n"))
    }
}

fswtch::api_callback! {
    fn bot_info_api(_cmd, _session, stream) {
        let Some(stream) = stream else {
            return fswtch::FALSE;
        };
        let Ok(reg) = REGISTRY.lock() else {
            return stream.write("registry lock poisoned\n");
        };
        if reg.is_empty() {
            return stream.write("no active calls\n");
        }
        let mut out = String::from("active calls:\n");
        for (uuid, state) in reg.iter() {
            let depth = state.lock().map(|s| s.tts_queue.len()).unwrap_or(0);
            out.push_str(&format!("  {uuid}: tts_queue={depth} samples\n"));
        }
        stream.write(&out)
    }
}

// ── module load: register the endpoint + APIs + the play_pcm subscription ───

fswtch::module_load! {
    fn switch_module_load(module) for "mod_vad_bot" {
        fswtch::log_info("mod_vad_bot", "loading module");
        EndpointIoBuilder::build::<VadBot>().and_then(|io| {
            let state_handler = StateHandlerTable::new_null();
            module
                .endpoint("fswtch_vad_bot", io, state_handler)
                .and_then(|m| {
                    m.api(
                        "fswtch_vad_bot_stop_playback",
                        "flushes the TTS play buffer on a bridged call (barge-in)",
                        "fswtch_vad_bot_stop_playback <uuid>",
                        stop_playback_api,
                    )
                })
                .and_then(|m| {
                    m.api(
                        "fswtch_vad_bot_info",
                        "lists active calls and TTS queue depths",
                        "fswtch_vad_bot_info",
                        bot_info_api,
                    )
                })
                .inspect(|_m| {
                    match fswtch::EventBinder::bind(
                        "mod_vad_bot.play",
                        fswtch::EventType::CUSTOM,
                        Some(PLAY_SUBCLASS),
                        Some(on_play_pcm),
                        std::ptr::null_mut(),
                    ) {
                        Ok(b) => std::mem::forget(b),
                        Err(e) => fswtch::log_error(
                            "mod_vad_bot",
                            format!("play_pcm bind failed: {e}"),
                        ),
                    }
                })
        })
    }
}
