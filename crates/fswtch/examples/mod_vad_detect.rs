//! mod_vad_detect — endpoint-based VAD + APM + TTS bridge (the bot IS the call terminus).
//!
//! The brain connects via outbound ESL socket, answers the call, and bridges to
//! `fswtch_vad_detect/<num>` (creates the B-leg). The endpoint's `read_frame`/
//! `write_frame` handle media: VAD runs on caller audio, TTS drains toward the caller.
//!
//! Features: VAD (per-segment, with pre-roll onset recovery + trailing-silence
//! trim), APM chain (HPF → AEC3 → NS → AGC2, each independently switchable), barge-in
//! (start-talking flushes the TTS queue), and the same ESL event contract.
//!
//! # Event contract
//! - `fswtch::vad` (VAD→brain): `Call-UUID`/`Vad-State`(start-talking|stop-talking)/`Seq`. No body.
//! - `fswtch::uplink_pcm` (VAD→brain): `Call-UUID`/`Seq`/`Sample-Rate`/`Channels`/
//!   `Bits-Per-Sample`(16)/`Sample-Format`(S16LE)/`Samples` + base64 PCM body. Per
//!   stop-talking segment; the start/stop frame shares `Seq` with the matching `fswtch::vad`.
//! - `fswtch::downlink_pcm` (brain→VAD): `Target-UUID`/`Sample-Rate`/`Channels`/
//!   `Bits-Per-Sample`/`Sample-Format` + base64 TTS PCM body → queued, drained by
//!   the endpoint's `read_frame`.
//!
//! # Use
//! ```text
//! load mod_vad_detect
//! <action application="export" data="FSWTCH_NS=12"/>
//! <action application="socket" data="127.0.0.1:8084 full"/>
//! # brain: connect → sendmsg answer → event plain ALL → sendmsg bridge fswtch_vad_detect/1000
//! #        → recv fswtch::vad/uplink_pcm → send fswtch::downlink_pcm
//! ```

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, LazyLock, Mutex};

use base64::{Engine, engine::general_purpose::STANDARD};
use fswtch::{
    CallDirection, CallerProfile, ChannelState, EndpointInterfaceRef, EndpointIoBuilder,
    EndpointIoRoutines, Frame, FrameMut, OriginateFlag, OutgoingResult, SUCCESS, Session,
    SpeechSegment, StateHandlerTable, Status, Vad, VadState, request_session, snap_segments,
};
use fswtch_apm::{EchoCanceller3, GainController2, HighPassFilter, NoiseSuppressor, NsLevel};

const VAD_SUBCLASS: &str = "fswtch::vad";
const UPLINK_SUBCLASS: &str = "fswtch::uplink_pcm";
const DOWNLINK_SUBCLASS: &str = "fswtch::downlink_pcm";
/// Play-queue high-water mark in seconds of audio — further `downlink_pcm` events are dropped.
const PLAY_QUEUE_MAX_SECS: u32 = 10;
/// Pipeline sample rate / packetization this module speaks (L16, 20 ms, mono).
const PIPELINE_RATE: u32 = 8000;
const FRAME_MS: u32 = 20;
const CHANNELS: u32 = 1;
/// APM 10 ms chunk: AEC3/NS/AGC2/HPF each process `rate/100 * channels` samples per call.
const APM_CHUNK: usize = (PIPELINE_RATE as usize / 100) * CHANNELS as usize;
/// FreeSWITCH `switch_signal_t` values: NONE=0, KILL=1, XFER=2, BREAK=3. Only KILL ends the call.
const SIG_KILL: i32 = 1;

fswtch::module_exports! {
    module = mod_vad_detect,
    load = switch_module_load,
}

// ── per-call state (shared by both modes) ───────────────────────────────────

struct CallState {
    vad: Vad,
    /// TTS samples (pipeline rate, mono) drained by the output path; filled by `on_downlink_pcm`.
    tts_queue: VecDeque<i16>,
    sample_rate: u32,
    channels: u32,
    /// Outbound sequence number (per call).
    seq: u64,
    /// Scratch for the mono downmix fed to `Vad::process` (reused per frame; no per-frame alloc).
    mono_scratch: Vec<i16>,
    /// Pre-roll buffer (silence before speech onset) — recovers what hysteresis truncates.
    pre_roll: VecDeque<i16>,
    /// Accumulated utterance PCM (pre_roll + talking frames) during speech.
    speech_buffer: Vec<i16>,
    /// Cap for `pre_roll` in samples (300 ms at the pipeline rate).
    pre_roll_max: usize,
    /// Frame counter for periodic debug logging (1 log per 50 frames ≈ 1/sec at 50 Hz).
    frame_count: u64,
    /// APM handles — `None` = disabled (passthrough). Controlled by channel variables
    /// `FSWTCH_AEC` / `FSWTCH_NS` / `FSWTCH_AGC2` / `FSWTCH_HPF` set in the dialplan.
    aec: Option<EchoCanceller3>,
    ns: Option<NoiseSuppressor>,
    agc2: Option<GainController2>,
    hpf: Option<HighPassFilter>,
}

// SAFETY: see mod_vad_bot — `Vad` and the four APM handles carry `PhantomData<*const ()>`
// and own C/C++ objects accessed exclusively under the per-call `Mutex`. All other fields
// are `Send`.
unsafe impl Send for CallState {}

static REGISTRY: LazyLock<Mutex<HashMap<String, Arc<Mutex<CallState>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Look up a call's state, dropping the registry lock before the caller touches the per-call lock.
fn lookup(uuid: &str) -> Option<Arc<Mutex<CallState>>> {
    REGISTRY.lock().ok()?.get(uuid).cloned()
}

// ── shared: create call state from A-leg session ─────────────────────────────

/// Read APM switches from the session's channel variables (`FSWTCH_AEC` / `FSWTCH_NS` /
/// `FSWTCH_AGC2` / `FSWTCH_HPF`, set via dialplan `export` on the A-leg) + construct a
/// `CallState` with VAD + enabled APM handles. Returns `None` if VAD init fails.
fn create_call_state(session: Option<&Session>) -> Option<CallState> {
    let (aec_enabled, ns_level, agc2_gain, hpf_enabled) = session
        .and_then(|s| s.channel())
        .map(|ch| {
            let aec = ch
                .variable("FSWTCH_AEC")
                .ok()
                .flatten()
                .is_some_and(|v| v == "true" || v == "1");
            let ns = ch
                .variable("FSWTCH_NS")
                .ok()
                .flatten()
                .and_then(|v| match v.as_str() {
                    "6" => Some(NsLevel::Db6),
                    "12" => Some(NsLevel::Db12),
                    "18" => Some(NsLevel::Db18),
                    "21" => Some(NsLevel::Db21),
                    _ => None,
                });
            let agc2 = ch
                .variable("FSWTCH_AGC2")
                .ok()
                .flatten()
                .and_then(|v| v.parse::<f32>().ok());
            let hpf = ch
                .variable("FSWTCH_HPF")
                .ok()
                .flatten()
                .is_some_and(|v| v == "true" || v == "1");
            (aec, ns, agc2, hpf)
        })
        .unwrap_or((false, None, None, false));
    let vad = match Vad::new(PIPELINE_RATE as i32, CHANNELS as i32) {
        Ok(vad) => vad,
        Err(error) => {
            fswtch::log_error("mod_vad_detect", format!("vad init failed: {error}"));
            return None;
        }
    };
    let aec = if aec_enabled {
        EchoCanceller3::new(PIPELINE_RATE as i32, CHANNELS as usize, CHANNELS as usize).ok()
    } else {
        None
    };
    let ns = ns_level
        .and_then(|l| NoiseSuppressor::new(l, PIPELINE_RATE as i32, CHANNELS as usize).ok());
    let agc2 = agc2_gain
        .and_then(|g| GainController2::new(g, true, PIPELINE_RATE as i32, CHANNELS as usize).ok());
    let hpf = if hpf_enabled {
        HighPassFilter::new(PIPELINE_RATE as i32, CHANNELS as usize).ok()
    } else {
        None
    };
    if aec.is_some() || ns.is_some() || agc2.is_some() || hpf.is_some() {
        fswtch::log_info(
            "mod_vad_detect",
            format!(
                "APM enabled: aec={} ns={} agc2={} hpf={}",
                aec.is_some(),
                ns.is_some(),
                agc2.is_some(),
                hpf.is_some()
            ),
        );
    }
    Some(CallState {
        vad,
        tts_queue: VecDeque::new(),
        sample_rate: PIPELINE_RATE,
        channels: CHANNELS,
        seq: 0,
        mono_scratch: Vec::new(),
        pre_roll: VecDeque::new(),
        speech_buffer: Vec::new(),
        pre_roll_max: (PIPELINE_RATE * 300 / 1000) as usize,
        frame_count: 0,
        aec,
        ns,
        agc2,
        hpf,
    })
}

// ── shared: process caller PCM (downmix → APM → VAD → accumulate) ───────────

/// Process one frame of caller PCM. Returns `(vad_label, seq, cleared_tts, speech_buffer)`
/// for event firing outside the lock. `vad_label` is `Some("start-talking")` /
/// `Some("stop-talking")` on boundaries, `None` for TALKING (accumulate only).
fn process_caller_pcm(
    s: &mut CallState,
    pcm: &[i16],
    channels: usize,
) -> (Option<&'static str>, u64, usize, Vec<i16>) {
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
    // APM capture chain: HPF → AEC3.process_capture → NS → AGC2 (each 10 ms chunk, in-place).
    for i in (0..s.mono_scratch.len()).step_by(APM_CHUNK) {
        let end = (i + APM_CHUNK).min(s.mono_scratch.len());
        if end - i != APM_CHUNK {
            break;
        }
        let half = &mut s.mono_scratch[i..end];
        if let Some(h) = s.hpf.as_mut() {
            let _ = h.process(half, CHANNELS as usize);
        }
        if let Some(a) = s.aec.as_mut() {
            let _ = a.process_capture(half, CHANNELS as usize, false);
        }
        if let Some(n) = s.ns.as_mut() {
            let _ = n.process(half, CHANNELS as usize);
        }
        if let Some(g) = s.agc2.as_mut() {
            let _ = g.process(half, CHANNELS as usize);
        }
    }
    let st = s.vad.process(&mut s.mono_scratch);
    // Periodic debug log: 1×/sec (every 50 frames at 50 Hz). Shows whether on_read is
    // being called, VAD state, and signal energy — enough to diagnose "no VAD events".
    s.frame_count = s.frame_count.wrapping_add(1);
    if s.frame_count == 1 || s.frame_count.is_multiple_of(50) {
        let vad_str = match st {
            VadState::NONE => "NONE",
            VadState::START_TALKING => "START",
            VadState::TALKING => "TALKING",
            VadState::STOP_TALKING => "STOP",
            _ => "ERROR",
        };
        let energy: u64 = s
            .mono_scratch
            .iter()
            .map(|&x| (x as i64 * x as i64) as u64)
            .sum::<u64>()
            / s.mono_scratch.len().max(1) as u64;
        fswtch::log_info(
            "mod_vad_detect",
            format!(
                "frame #{}: pcm={} vad={} energy={} pre_roll={} buf={}",
                s.frame_count,
                s.mono_scratch.len(),
                vad_str,
                energy,
                s.pre_roll.len(),
                s.speech_buffer.len()
            ),
        );
    }
    let label = match st {
        VadState::START_TALKING => Some("start-talking"),
        VadState::STOP_TALKING => Some("stop-talking"),
        VadState::TALKING => None,
        _ => {
            // NONE / ERROR: push to pre_roll (onset recovery) + don't fire.
            s.pre_roll.extend(s.mono_scratch.iter().copied());
            while s.pre_roll.len() > s.pre_roll_max {
                s.pre_roll.pop_front();
            }
            return (None, 0, 0, Vec::new());
        }
    };
    s.seq = s.seq.wrapping_add(1);
    match label {
        Some("start-talking") => {
            // Onset recovery: drain pre_roll into speech_buffer + this frame.
            s.speech_buffer.clear();
            let pre_roll: Vec<i16> = s.pre_roll.drain(..).collect();
            s.speech_buffer.extend(pre_roll);
            s.speech_buffer.extend(s.mono_scratch.iter().copied());
            // Barge-in: flush queued TTS so the caller's speech isn't drowned.
            let cleared = s.tts_queue.len();
            s.tts_queue.clear();
            (label, s.seq, cleared, Vec::new())
        }
        Some("stop-talking") => {
            s.speech_buffer.extend(s.mono_scratch.iter().copied());
            // Take the buffer out — snap + fire outside the lock.
            let buf = std::mem::take(&mut s.speech_buffer);
            (label, s.seq, 0, buf)
        }
        _ => {
            // TALKING: accumulate.
            s.speech_buffer.extend(s.mono_scratch.iter().copied());
            (label, s.seq, 0, Vec::new())
        }
    }
}

// ── shared: produce TTS output (drain queue → fill → AEC render) ────────────

/// Fill `buf` with TTS from the queue; silence (zero) on underrun. Feeds the played
/// audio to AEC3's `analyze_render` for echo cancellation. The caller must already
/// hold the per-call lock.
fn produce_tts_output(s: &mut CallState, buf: &mut [i16]) {
    let queue_before = s.tts_queue.len();
    for slot in buf.iter_mut() {
        *slot = s.tts_queue.pop_front().unwrap_or(0); // underrun → silence
    }
    // Periodic debug log for the TTS output path (same cadence as process_caller_pcm).
    if s.frame_count == 1 || s.frame_count.is_multiple_of(50) {
        fswtch::log_info(
            "mod_vad_detect",
            format!(
                "tts out #{}: buf={} queue_before={} queue_after={}",
                s.frame_count,
                buf.len(),
                queue_before,
                s.tts_queue.len()
            ),
        );
    }
    // Feed the played TTS (far-end render) to AEC3 for echo cancellation.
    if let Some(aec) = s.aec.as_mut() {
        for chunk in buf.chunks(APM_CHUNK) {
            if chunk.len() == APM_CHUNK {
                let _ = aec.analyze_render(chunk, CHANNELS as usize);
            }
        }
    }
}

// ── shared: fire events outside the lock ─────────────────────────────────────

fn fire_vad(uuid: &str, vad_state: &str, seq: u64) -> fswtch::Result<()> {
    let mut ev = fswtch::Event::custom(VAD_SUBCLASS)?;
    ev.add_header("Call-UUID", uuid)?;
    ev.add_header("Vad-State", vad_state)?;
    ev.add_header("Seq", &seq.to_string())?;
    ev.fire()
}

#[allow(clippy::too_many_arguments)] // six header fields map 1:1 onto add_header calls
fn fire_uplink_pcm(
    uuid: &str,
    seq: u64,
    sample_rate: u32,
    channels: u32,
    samples: u32,
    body: &str,
) -> fswtch::Result<()> {
    let mut ev = fswtch::Event::custom(UPLINK_SUBCLASS)?;
    ev.add_header("Call-UUID", uuid)?;
    ev.add_header("Seq", &seq.to_string())?;
    ev.add_header("Sample-Rate", &sample_rate.to_string())?;
    ev.add_header("Channels", &channels.to_string())?;
    ev.add_header("Bits-Per-Sample", "16")?;
    ev.add_header("Sample-Format", "S16LE")?;
    ev.add_header("Samples", &samples.to_string())?;
    ev.add_body(body)?;
    ev.fire()
}

/// Fire `fswtch::vad` (start/stop) + snap + fire `fswtch::uplink_pcm` (segment) outside the lock.
fn fire_events(
    uuid: &str,
    vad_label: Option<&str>,
    seq: u64,
    cleared_tts: usize,
    speech_buffer: Vec<i16>,
) {
    if cleared_tts > 0 {
        fswtch::log_info(
            "mod_vad_detect",
            format!("barge-in on {uuid}: cleared {cleared_tts} TTS samples"),
        );
    }
    if let Some(label) = vad_label
        && let Err(error) = fire_vad(uuid, label, seq)
    {
        fswtch::log_error("mod_vad_detect", format!("fire vad failed: {error}"));
    }
    if !speech_buffer.is_empty() {
        let frame_samples = (PIPELINE_RATE * FRAME_MS / 1000) as usize;
        let mut segs = vec![SpeechSegment {
            start_sample: 0,
            end_sample: speech_buffer.len(),
        }];
        snap_segments(&speech_buffer, frame_samples, &mut segs);
        let segment = segs
            .first()
            .map(|seg| seg.samples(&speech_buffer).to_vec())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| speech_buffer.clone());
        fswtch::log_info(
            "mod_vad_detect",
            format!(
                "uplink_pcm segment on {uuid}: {} samples ({:.1}s)",
                segment.len(),
                segment.len() as f32 / PIPELINE_RATE as f32
            ),
        );
        let body = STANDARD.encode(
            segment
                .iter()
                .flat_map(|&s| s.to_le_bytes())
                .collect::<Vec<u8>>(),
        );
        if let Err(error) =
            fire_uplink_pcm(uuid, seq, PIPELINE_RATE, 1, segment.len() as u32, &body)
        {
            fswtch::log_error("mod_vad_detect", format!("fire uplink_pcm failed: {error}"));
        }
    }
}

// ── endpoint mode ───────────────────────────────────────────────────────────

pub struct VadDetectEndpoint;

impl EndpointIoRoutines for VadDetectEndpoint {
    const NAME: &'static str = "fswtch_vad_detect";

    /// Create the B leg when the dialplan bridges to `fswtch_vad_detect/<num>`.
    fn outgoing_channel(
        session: Option<&Session>,
        caller_profile: Option<CallerProfile>,
        endpoint: &EndpointInterfaceRef,
        flags: OriginateFlag,
    ) -> OutgoingResult {
        let Some(new_session) = request_session(endpoint, CallDirection::OUTBOUND, flags) else {
            fswtch::log_error("mod_vad_detect", "outgoing_channel: request_session failed");
            return OutgoingResult::refused();
        };
        let Some(channel) = new_session.channel() else {
            fswtch::log_error("mod_vad_detect", "outgoing_channel: no channel");
            new_session.hangup(fswtch::Cause::DESTINATION_OUT_OF_ORDER);
            return OutgoingResult::refused();
        };

        if let Some(ref profile) = caller_profile {
            channel.set_caller_profile(profile);
        }
        let _ = channel.set_name("fswtch_vad_detect");
        let _ = channel.mark_answered();
        channel.set_audio_flag();
        if let Err(error) = new_session.init_read_codec("L16", PIPELINE_RATE, FRAME_MS, CHANNELS) {
            fswtch::log_error("mod_vad_detect", format!("init_read_codec failed: {error}"));
        }
        if let Err(error) = new_session.init_write_codec("L16", PIPELINE_RATE, FRAME_MS, CHANNELS) {
            fswtch::log_error(
                "mod_vad_detect",
                format!("init_write_codec failed: {error}"),
            );
        }
        // Drive the leg out of CS_NEW. CS_CONSUME_MEDIA is the terminal "wait for media"
        // state; our all-NULL state-handler table lets the standard on_consume_media handler
        // run (a no-op), then the thread sleeps on its condvar until hangup. The A leg's
        // bridge then drives read/write frames through our I/O routines.
        channel.set_state(ChannelState::CONSUME_MEDIA);

        let uuid = channel.uuid().unwrap_or_default();
        if uuid.is_empty() {
            fswtch::log_error("mod_vad_detect", "outgoing_channel: channel has no uuid");
            new_session.hangup(fswtch::Cause::DESTINATION_OUT_OF_ORDER);
            return OutgoingResult::refused();
        }
        let Some(state) = create_call_state(session) else {
            fswtch::log_error(
                "mod_vad_detect",
                "outgoing_channel: create_call_state failed",
            );
            new_session.hangup(fswtch::Cause::DESTINATION_OUT_OF_ORDER);
            return OutgoingResult::refused();
        };
        let state = Arc::new(Mutex::new(state));
        if let Ok(mut reg) = REGISTRY.lock() {
            reg.insert(uuid.clone(), state);
            fswtch::log_info("mod_vad_detect", format!("call registered: {uuid}"));
        }
        fswtch::log_info(
            "mod_vad_detect",
            format!("outgoing_channel: created session {uuid}"),
        );
        OutgoingResult::success(new_session)
    }

    /// FreeSWITCH writes the CALLER'S audio TO this endpoint. VAD runs here.
    fn write_frame(session: &Session, frame: &Frame) -> Status {
        let Some(uuid) = session.channel().and_then(|c| c.uuid()) else {
            return SUCCESS;
        };
        let Some(pcm) = frame.pcm_i16() else {
            return SUCCESS;
        };
        let channels = frame.channels().max(1) as usize;
        let Some(state) = lookup(&uuid) else {
            return SUCCESS;
        };

        let (vad_label, seq, cleared_tts, speech_buffer) = {
            let mut guard = match state.lock() {
                Ok(g) => g,
                Err(_) => return SUCCESS,
            };
            process_caller_pcm(&mut guard, pcm, channels)
        };
        fire_events(&uuid, vad_label, seq, cleared_tts, speech_buffer);
        SUCCESS
    }

    /// FreeSWITCH reads audio FROM this endpoint (toward the caller). Drain TTS queue;
    /// silence when empty. The frame is ALWAYS filled.
    fn read_frame(session: &Session, frame: &mut FrameMut) -> Status {
        let Some(buf) = frame.pcm_i16_output() else {
            return SUCCESS;
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
            Ok(mut s) => produce_tts_output(&mut s, buf),
            Err(_) => buf.fill(0),
        }
        SUCCESS
    }

    /// Hangup signal. Only `SIG_KILL` ends the call.
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
            fswtch::log_info("mod_vad_detect", format!("call ended: {uuid}"));
        }
        SUCCESS
    }
}

// ── inbound: fswtch::downlink_pcm (base64 TTS PCM) → TTS queue ──────────────

fswtch::event_callback! {
    fn on_downlink_pcm(event) {
        let target = match event.header("Target-UUID") {
            Some(t) if !t.is_empty() => t,
            _ => {
                fswtch::log_error("mod_vad_detect", "downlink_pcm event missing Target-UUID");
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
                    "mod_vad_detect",
                    format!("downlink_pcm base64 decode failed: {error}"),
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
            fswtch::log_error("mod_vad_detect", format!("no active call on {target}"));
            return;
        };
        match state.lock() {
            Ok(mut s) => {
                if let Some(rate) = rate_hdr
                    && rate != s.sample_rate
                {
                    fswtch::log_error(
                        "mod_vad_detect",
                        format!(
                            "downlink_pcm rate mismatch on {target}: got {rate}, want {}",
                            s.sample_rate
                        ),
                    );
                    return;
                }
                if let Some(ch) = chan_hdr && ch != s.channels {
                    fswtch::log_error(
                        "mod_vad_detect",
                        format!(
                            "downlink_pcm channels mismatch on {target}: got {ch}, want {}",
                            s.channels
                        ),
                    );
                    return;
                }
                let max_samples = s.sample_rate.saturating_mul(PLAY_QUEUE_MAX_SECS) as usize;
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
            Err(_) => fswtch::log_error("mod_vad_detect", "call lock poisoned"),
        }
    }
}

// ── APIs ────────────────────────────────────────────────────────────────────

fswtch::api_callback! {
    fn stop_playback_api(cmd, _session, stream) {
        fswtch::log_info("mod_vad_detect", "fswtch_vad_detect_stop_playback invoked");
        let Some(stream) = stream else {
            return fswtch::FALSE;
        };
        let uuid = cmd.unwrap_or_default().trim().to_owned();
        if uuid.is_empty() {
            return stream.write("usage: fswtch_vad_detect_stop_playback <uuid>\n");
        }
        let Some(state) = lookup(&uuid) else {
            return stream.write(&format!("no active call on {uuid}\n"));
        };
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
    fn detect_info_api(_cmd, _session, stream) {
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

// ── module load: register endpoint + application + APIs + downlink_pcm ──────

fswtch::module_load! {
    fn switch_module_load(module) for "mod_vad_detect" {
        fswtch::log_info("mod_vad_detect", "loading module");
        EndpointIoBuilder::build::<VadDetectEndpoint>().and_then(|io| {
            let state_handler = StateHandlerTable::new_null();
            module
                .endpoint("fswtch_vad_detect", io, state_handler)
                .and_then(|m| {
                    m.api(
                        "fswtch_vad_detect_stop_playback",
                        "flushes the TTS play buffer on a call (barge-in)",
                        "fswtch_vad_detect_stop_playback <uuid>",
                        stop_playback_api,
                    )
                })
                .and_then(|m| {
                    m.api(
                        "fswtch_vad_detect_info",
                        "lists active calls and TTS queue depths",
                        "fswtch_vad_detect_info",
                        detect_info_api,
                    )
                })
                .inspect(|_m| {
                    match fswtch::EventBinder::bind(
                        "mod_vad_detect.downlink",
                        fswtch::EventType::CUSTOM,
                        Some(DOWNLINK_SUBCLASS),
                        Some(on_downlink_pcm),
                        std::ptr::null_mut(),
                    ) {
                        Ok(b) => std::mem::forget(b),
                        Err(e) => fswtch::log_error(
                            "mod_vad_detect",
                            format!("downlink_pcm bind failed: {e}"),
                        ),
                    }
                })
        })
    }
}
