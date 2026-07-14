//! mod_vad_detect — VAD media-bug module (READ_REPLACE + WRITE_REPLACE).
//!
//! The dialplan attaches this as a media bug on the A-leg, then enters
//! ``socket async full`` which connects the brain AND auto-parks (driving
//! the read/write frame loop).  No endpoint, no B-leg, no ``sendmsg park``.
//!
//! # Event contract
//! - `fswtch::vad` (VAD→brain): `Call-UUID`/`Unique-ID`/`Vad-State`/`Seq`. No body.
//! - `fswtch::uplink_pcm` (VAD→brain): `Call-UUID`/`Unique-ID`/`Seq`/audio headers + base64 PCM body.
//! - `fswtch::downlink_pcm` (brain→VAD): `Target-UUID`/audio headers + base64 TTS PCM body.
//!
//! # Use
//! ```text
//! load mod_vad_detect
//! <action application="fswtch_vad_detect"/>
//! <action application="socket" data="127.0.0.1:8084 async full"/>
//! ```

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, LazyLock, Mutex};

use base64::{Engine, engine::general_purpose::STANDARD};
use fswtch::{
    ApplicationInfo, MediaBugAction, MediaBugConfig, MediaBugContext, MediaBugFlags,
    MediaBugHandler, MediaFrameMut, Session, SpeechSegment, Vad, VadState, snap_segments,
};

const VAD_SUBCLASS: &str = "fswtch::vad";
const UPLINK_SUBCLASS: &str = "fswtch::uplink_pcm";
const DOWNLINK_SUBCLASS: &str = "fswtch::downlink_pcm";
const PLAY_QUEUE_MAX_SECS: u32 = 10;
const PIPELINE_RATE: u32 = 8000;
const FRAME_MS: u32 = 20;
const CHANNELS: u32 = 1;

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

// ── media bug handler ───────────────────────────────────────────────────────

struct VadDetectBug {
    uuid: String,
    state: Arc<Mutex<CallState>>,
    read_frames: u64,
    write_frames: u64,
}

impl MediaBugHandler for VadDetectBug {
    fn on_init(&mut self, _ctx: &mut MediaBugContext<'_>) -> MediaBugAction {
        match REGISTRY.lock() {
            Ok(mut reg) => {
                reg.insert(self.uuid.clone(), Arc::clone(&self.state));
                fswtch::log_info(
                    "mod_vad_detect",
                    format!("bug registered for {}", self.uuid),
                );
            }
            Err(_) => fswtch::log_error("mod_vad_detect", "registry lock poisoned on init"),
        }
        MediaBugAction::Continue
    }

    fn on_read_replace(
        &mut self,
        _ctx: &mut MediaBugContext<'_>,
        mut frame: MediaFrameMut<'_>,
    ) -> MediaBugAction {
        self.read_frames = self.read_frames.wrapping_add(1);
        let channels = frame.as_frame().channels().max(1) as usize;
        let pcm = frame.pcm_i16_mut();
        let Some(pcm) = pcm else {
            return MediaBugAction::Continue;
        };
        let (vad_label, seq, cleared_tts, speech_buffer) = {
            let mut s = match self.state.lock() {
                Ok(s) => s,
                Err(_) => return MediaBugAction::Continue,
            };
            process_caller_pcm(&mut s, pcm, channels)
        };
        fire_events(&self.uuid, vad_label, seq, cleared_tts, speech_buffer);
        MediaBugAction::Continue
    }

    fn on_write_replace(
        &mut self,
        _ctx: &mut MediaBugContext<'_>,
        mut frame: MediaFrameMut<'_>,
    ) -> MediaBugAction {
        self.write_frames = self.write_frames.wrapping_add(1);
        let Some(buf) = frame.pcm_i16_mut() else {
            return MediaBugAction::Continue;
        };
        let mut s = match self.state.lock() {
            Ok(s) => s,
            Err(_) => return MediaBugAction::Continue,
        };
        if s.tts_queue.is_empty() {
            return MediaBugAction::Continue;
        }
        produce_tts_output(&mut s, buf);
        MediaBugAction::Continue
    }

    fn on_close(&mut self, _ctx: &mut MediaBugContext<'_>) {
        match REGISTRY.lock() {
            Ok(mut reg) => {
                reg.remove(&self.uuid);
                fswtch::log_info("mod_vad_detect", format!("bug closed for {}", self.uuid));
            }
            Err(_) => fswtch::log_error("mod_vad_detect", "registry lock poisoned on close"),
        }
    }
}

// ── app entry (dialplan: <action application="fswtch_vad_detect"/>) ────────

fswtch::app_callback! {
    fn vad_detect_app(session, _data) {
        fswtch::log_info("mod_vad_detect", "application invoked");
        let Some(session) = session else {
            fswtch::log_error("mod_vad_detect", "missing session");
            return;
        };
        let uuid = session.channel().and_then(|c| c.uuid()).unwrap_or_default();
        if uuid.is_empty() {
            fswtch::log_error("mod_vad_detect", "no uuid");
            return;
        }
        let vad = match Vad::new(PIPELINE_RATE as i32, CHANNELS as i32) {
            Ok(vad) => vad,
            Err(error) => {
                fswtch::log_error("mod_vad_detect", format!("vad init failed: {error}"));
                return;
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
        let config = match MediaBugConfig::new(
            "fswtch_vad_detect",
            "read-write-replace",
            MediaBugFlags::READ_REPLACE | MediaBugFlags::WRITE_REPLACE | MediaBugFlags::NO_PAUSE,
        ) {
            Ok(c) => c,
            Err(e) => {
                fswtch::log_error("mod_vad_detect", format!("media bug config failed: {e}"));
                return;
            }
        };
        let handler = VadDetectBug {
            uuid: uuid.clone(),
            state,
            read_frames: 0,
            write_frames: 0,
        };
        if let Err(error) = fswtch::attach_media_bug(session, config, handler) {
            fswtch::log_error("mod_vad_detect", format!("attach media bug failed: {error}"));
        }
    }
}

// ── inbound: fswtch::downlink_pcm → TTS queue ──────────────────────────────

fswtch::event_callback! {
    fn on_downlink_pcm(event) {
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
        module
            .application(
                ApplicationInfo::new(
                    "fswtch_vad_detect",
                    "VAD media bug: READ_REPLACE (VAD on caller audio) + \
                     WRITE_REPLACE (TTS drain). Use with 'socket async full' to \
                     auto-park and drive media.",
                    "Rust VAD detect",
                    "fswtch_vad_detect",
                ),
                vad_detect_app,
            )
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
