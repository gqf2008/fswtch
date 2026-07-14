//! mod_vad_detect — VAD + TTS bridge (endpoint mode, bridge-driven).
//!
//! The brain connects via ``socket async full`` and sends
//! ``sendmsg bridge fswtch_vad_detect/1000`` to create the B-leg.
//! The bridge drives ``write_frame`` (caller audio → VAD → fire events) and
//! ``read_frame`` (drain TTS → caller). No media bug, no playback, no park CNG issue.
//!
//! # Event contract
//! - `fswtch::vad` (VAD→brain): `Call-UUID`/`Unique-ID`/`Vad-State`/`Seq`. No body.
//! - `fswtch::uplink_pcm` (VAD→brain): `Call-UUID`/`Unique-ID`/`Seq`/audio headers + base64 PCM body.
//! - `fswtch::downlink_pcm` (brain→VAD): `Target-UUID`/audio headers + base64 TTS PCM body.
//!
//! # Use
//! ```text
//! load mod_vad_detect
//! <action application="socket" data="127.0.0.1:8084 async full"/>
//! # brain: connect → api uuid_answer → event plain ALL → sendmsg bridge fswtch_vad_detect/1000
//! ```

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, LazyLock, Mutex};

use base64::{Engine, engine::general_purpose::STANDARD};
use fswtch::{
    CallDirection, CallerProfile, ChannelState, EndpointInterfaceRef, EndpointIoBuilder,
    EndpointIoRoutines, Frame, FrameMut, OriginateFlag, OutgoingResult, SUCCESS, Session,
    SpeechSegment, StateHandlerTable, Status, Vad, VadState, request_session, snap_segments,
};

const VAD_SUBCLASS: &str = "fswtch::vad";
const UPLINK_SUBCLASS: &str = "fswtch::uplink_pcm";
const DOWNLINK_SUBCLASS: &str = "fswtch::downlink_pcm";
const PLAY_QUEUE_MAX_SECS: u32 = 10;
const PIPELINE_RATE: u32 = 8000;
const FRAME_MS: u32 = 20;
const CHANNELS: u32 = 1;
/// FreeSWITCH `switch_signal_t` values: NONE=0, KILL=1, XFER=2, BREAK=3. Only KILL ends the call.
const SIG_KILL: i32 = 1;

fswtch::module_exports! {
    module = mod_vad_detect,
    load = switch_module_load,
}

// ── per-call state ──────────────────────────────────────────────────────────

struct CallState {
    vad: Vad,
    tts_queue: VecDeque<i16>,
    seq: u64,
    mono_scratch: Vec<i16>,
    pre_roll: VecDeque<i16>,
    speech_buffer: Vec<i16>,
    pre_roll_max: usize,
    frame_count: u64,
}

// SAFETY: `CallState`'s only non-`Send` field is `Vad`, which is `!Send` by marker-convention
// (`PhantomData<*const ()>`) — `switch_vad_t` is a plain heap struct with no thread affinity,
// only mutated through `&self` (see `Vad::process`). Every other field is plain `Send` heap data
// (`Vec`/`VecDeque`). `CallState` is only reached through `Arc<Mutex<..>>` and mutated under the
// lock, so there is never concurrent or cross-thread access without synchronization.
unsafe impl Send for CallState {}

static REGISTRY: LazyLock<Mutex<HashMap<String, Arc<Mutex<CallState>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn lookup(uuid: &str) -> Option<Arc<Mutex<CallState>>> {
    REGISTRY.lock().ok()?.get(uuid).cloned()
}

// ── shared processing ──────────────────────────────────────────────────────

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
    let st = s.vad.process(&mut s.mono_scratch);
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
            s.speech_buffer.clear();
            let pre_roll: Vec<i16> = s.pre_roll.drain(..).collect();
            s.speech_buffer.extend(pre_roll);
            s.speech_buffer.extend(s.mono_scratch.iter().copied());
            let cleared = s.tts_queue.len();
            s.tts_queue.clear();
            (label, s.seq, cleared, Vec::new())
        }
        Some("stop-talking") => {
            s.speech_buffer.extend(s.mono_scratch.iter().copied());
            let buf = std::mem::take(&mut s.speech_buffer);
            (label, s.seq, 0, buf)
        }
        _ => {
            s.speech_buffer.extend(s.mono_scratch.iter().copied());
            (label, s.seq, 0, Vec::new())
        }
    }
}

fn produce_tts_output(s: &mut CallState, buf: &mut [i16]) {
    for slot in buf.iter_mut() {
        *slot = s.tts_queue.pop_front().unwrap_or(0);
    }
    if s.frame_count == 1 || s.frame_count.is_multiple_of(50) {
        fswtch::log_info(
            "mod_vad_detect",
            format!(
                "tts out #{}: buf={} queue={}",
                s.frame_count,
                buf.len(),
                s.tts_queue.len()
            ),
        );
    }
}

// ── event firing ────────────────────────────────────────────────────────────

fn fire_vad(uuid: &str, vad_state: &str, seq: u64) -> fswtch::Result<()> {
    let mut ev = fswtch::Event::custom(VAD_SUBCLASS)?;
    ev.add_header("Unique-ID", uuid)?;
    ev.add_header("Call-UUID", uuid)?;
    ev.add_header("Vad-State", vad_state)?;
    ev.add_header("Seq", &seq.to_string())?;
    ev.fire()
}

#[allow(clippy::too_many_arguments)]
fn fire_uplink_pcm(uuid: &str, seq: u64, samples: u32, body: &str) -> fswtch::Result<()> {
    let mut ev = fswtch::Event::custom(UPLINK_SUBCLASS)?;
    ev.add_header("Unique-ID", uuid)?;
    ev.add_header("Call-UUID", uuid)?;
    ev.add_header("Seq", &seq.to_string())?;
    ev.add_header("Sample-Rate", &PIPELINE_RATE.to_string())?;
    ev.add_header("Channels", &CHANNELS.to_string())?;
    ev.add_header("Bits-Per-Sample", "16")?;
    ev.add_header("Sample-Format", "S16LE")?;
    ev.add_header("Samples", &samples.to_string())?;
    ev.add_body(body)?;
    ev.fire()
}

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
        if let Err(error) = fire_uplink_pcm(uuid, seq, segment.len() as u32, &body) {
            fswtch::log_error("mod_vad_detect", format!("fire uplink_pcm failed: {error}"));
        }
    }
}

// ── endpoint mode (bridge-driven) ──────────────────────────────────────────

pub struct VadDetectEndpoint;

impl EndpointIoRoutines for VadDetectEndpoint {
    const NAME: &'static str = "fswtch_vad_detect";

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
        channel.set_state(ChannelState::CONSUME_MEDIA);
        let uuid = channel.uuid().unwrap_or_default();
        if uuid.is_empty() {
            fswtch::log_error("mod_vad_detect", "outgoing_channel: channel has no uuid");
            new_session.hangup(fswtch::Cause::DESTINATION_OUT_OF_ORDER);
            return OutgoingResult::refused();
        }
        let vad = match Vad::new(PIPELINE_RATE as i32, CHANNELS as i32) {
            Ok(vad) => vad,
            Err(error) => {
                fswtch::log_error("mod_vad_detect", format!("vad init failed: {error}"));
                new_session.hangup(fswtch::Cause::DESTINATION_OUT_OF_ORDER);
                return OutgoingResult::refused();
            }
        };
        let state = Arc::new(Mutex::new(CallState {
            vad,
            tts_queue: VecDeque::new(),
            seq: 0,
            mono_scratch: Vec::new(),
            pre_roll: VecDeque::new(),
            speech_buffer: Vec::new(),
            pre_roll_max: (PIPELINE_RATE * 300 / 1000) as usize,
            frame_count: 0,
        }));
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

    /// Caller audio TO this endpoint — VAD runs here.
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

    /// Audio FROM this endpoint TO caller — drain TTS queue. Always filled.
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

// ── inbound: fswtch::downlink_pcm → TTS queue ──────────────────────────────

fswtch::event_callback! {
    fn on_downlink_pcm(event) {
        fswtch::log_info("mod_vad_detect", "on_downlink_pcm callback FIRED");
        let target = match event.header("Target-UUID") {
            Some(t) if !t.is_empty() => t,
            _ => {
                fswtch::log_error("mod_vad_detect", "downlink_pcm event missing Target-UUID");
                return;
            }
        };
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
                let max_samples = PIPELINE_RATE.saturating_mul(PLAY_QUEUE_MAX_SECS) as usize;
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
        let Some(stream) = stream else { return fswtch::FALSE };
        let uuid = cmd.unwrap_or_default().trim().to_owned();
        if uuid.is_empty() {
            return stream.write("usage: fswtch_vad_detect_stop_playback <uuid>\n");
        }
        let Some(state) = lookup(&uuid) else {
            return stream.write(&format!("no active call on {uuid}\n"));
        };
        let cleared = match state.lock() {
            Ok(mut s) => { let n = s.tts_queue.len(); s.tts_queue.clear(); n }
            Err(_) => return stream.write("call lock poisoned\n"),
        };
        stream.write(&format!("cleared {cleared} queued samples on {uuid}\n"))
    }
}

fswtch::api_callback! {
    fn detect_info_api(_cmd, _session, stream) {
        let Some(stream) = stream else { return fswtch::FALSE };
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

// ── module load ─────────────────────────────────────────────────────────────

fswtch::module_load! {
    fn switch_module_load(module) for "mod_vad_detect" {
        fswtch::log_info("mod_vad_detect", "loading module");
        EndpointIoBuilder::build::<VadDetectEndpoint>()
            .and_then(|io| {
                let state_handler = StateHandlerTable::new_null();
                module.endpoint("fswtch_vad_detect", io, state_handler)
            })
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
    }
}
