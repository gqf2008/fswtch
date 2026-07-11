//! mod_vad_pcm — bidirectional raw-PCM ESL bridge (merges mod_vad_detect + mod_vad_esl).
//!
//! A thin media-bug bridge: PCM rides the event body (base64), audio metadata (sample rate
//! / channels / bits / format) rides the event headers. No FS ASR (`detect_speech`) and no FS
//! TTS (`speak`) — ASR/TTS are entirely downstream's job; this module only ferries PCM frames.
//! No files, no HTTP.
//!
//! # Outbound — VAD → `fswtch::asr_result` (per talking frame)
//! A media bug (`READ_STREAM | WRITE_REPLACE | NO_PAUSE`) is attached to the session. `on_read`
//! runs `fswtch::Vad` over each read frame's `pcm_i16()`. For every talking-active frame
//! (START_TALKING / TALKING / STOP_TALKING) it fires a custom `fswtch::asr_result` event whose
//! body is the frame's PCM base64-encoded, with headers `Call-UUID`, `Vad-State`, `Seq`,
//! `Sample-Rate`, `Channels`, `Bits-Per-Sample` (16), `Sample-Format` (S16LE), `Samples`.
//! Silence (NONE) and errors are not forwarded.
//!
//! # Inbound — `fswtch::play_pcm` → write-replace playback
//! At load the module subscribes to custom `fswtch::play_pcm` events (an ESL socket sends
//! `sendevent CUSTOM` with `Event-Subclass: fswtch::play_pcm`). The event carries
//! `Target-UUID` + the same audio headers + a base64 PCM body. The decoded samples are routed
//! by uuid into that session's play queue; the same bug's `on_write_replace` drains the queue
//! into the outgoing frame (silence-padding on underrun, pass-through when idle).
//!
//! # Format contract
//! Fixed 16-bit linear (SLIN), declared `S16LE` (FS SLIN is native-endian, == LE on the common
//! little-endian hosts). Telephony mono is assumed: VAD runs on a mono downmix when channels >
//! 1, but the outbound body still carries the original interleaved PCM with the real `Channels`
//! header. Inbound expects mono matching the session's write rate; mismatches are logged and
//! dropped (resampling/stereo is left as a downstream concern — downstream learns the session
//! format from the outbound events and should echo it back).
//!
//! # Use
//! ```text
//! load mod_vad_pcm
//! # dialplan: attach the bridge on an answered call
//! <action application="rust_vad_pcm"/>
//! # or fs_cli against an existing call:
//! fs_cli -x 'rust_vad_pcm_start <uuid>'
//! # downstream brain: subscribe `event custom fswtch::asr_result`, base64-decode each body to
//! # S16LE PCM at the advertised Sample-Rate/Channels, run ASR, then TTS and send each chunk
//! # back as a fswtch::play_pcm event (same headers + Target-UUID + base64 PCM body).
//! # on barge-in (caller interrupts the bot): flush the play buffer to stop TTS at once:
//! fs_cli -x 'rust_vad_pcm_stop_playback <uuid>'
//! ```

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, LazyLock, Mutex};

use base64::{Engine, engine::general_purpose::STANDARD};
use fswtch::{
    MediaBugAction, MediaBugConfig, MediaBugContext, MediaBugFlags, MediaBugHandler, MediaFrame,
    MediaFrameMut, Session, SessionGuard, Vad, VadState,
};

const ASR_SUBCLASS: &str = "fswtch::asr_result";
const PLAY_SUBCLASS: &str = "fswtch::play_pcm";
/// Play-queue high-water mark in seconds of audio — further `play_pcm` events are dropped.
const PLAY_QUEUE_MAX_SECS: u32 = 10;

fswtch::module_exports! {
    module = mod_vad_pcm,
    load = switch_module_load,
}

// ── per-session bridge state shared across threads ──────────────────────────
//
// The play queue is the only piece of state that crosses threads: the ESL event worker
// (`on_play_pcm`) pushes, the session's media thread (`on_write_replace`) drains. The VAD lives
// only in the handler (media thread), so it stays out of this struct and needs no lock.

struct BridgeState {
    queue: Mutex<VecDeque<i16>>,
    sample_rate: u32,
    channels: u32,
}

static REGISTRY: LazyLock<Mutex<HashMap<String, Arc<BridgeState>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

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

// ── the media-bug handler: VAD on read, playback drain on write-replace ──────

struct PcmBridge {
    vad: Vad,
    uuid: String,
    state: Arc<BridgeState>,
    seq: u64,
    mono_scratch: Vec<i16>,
}

impl MediaBugHandler for PcmBridge {
    fn on_init(&mut self, _ctx: &mut MediaBugContext<'_>) -> MediaBugAction {
        match REGISTRY.lock() {
            Ok(mut reg) => {
                reg.insert(self.uuid.clone(), Arc::clone(&self.state));
                fswtch::log_info(
                    "mod_vad_pcm",
                    format!("bridge registered for {}", self.uuid),
                );
            }
            Err(_) => fswtch::log_error("mod_vad_pcm", "registry lock poisoned on init"),
        }
        MediaBugAction::Continue
    }

    fn on_read(&mut self, _ctx: &mut MediaBugContext<'_>, frame: MediaFrame<'_>) -> MediaBugAction {
        let Some(pcm) = frame.pcm_i16() else {
            return MediaBugAction::Continue; // non-16-bit-linear frame: nothing to VAD
        };
        let channels = frame.channels().max(1) as usize;
        // VAD is mono: feed the whole slice for mono, or a per-frame channel-average for stereo.
        self.mono_scratch.clear();
        if channels == 1 {
            self.mono_scratch.extend_from_slice(pcm);
        } else {
            self.mono_scratch.reserve(pcm.len() / channels);
            for chunk in pcm.chunks_exact(channels) {
                let sum: i64 = chunk.iter().map(|&s| s as i64).sum();
                self.mono_scratch.push((sum / channels as i64) as i16);
            }
        }
        let state = self.vad.process(&mut self.mono_scratch);
        let label = match state {
            VadState::START_TALKING => "start-talking",
            VadState::TALKING => "talking",
            VadState::STOP_TALKING => "stop-talking",
            _ => return MediaBugAction::Continue, // NONE / ERROR: silence is not forwarded
        };
        self.seq = self.seq.wrapping_add(1);
        let body = STANDARD.encode(frame.bytes());
        if let Err(error) = fire_asr(
            &self.uuid,
            label,
            self.seq,
            frame.rate(),
            frame.channels(),
            frame.samples(),
            &body,
        ) {
            fswtch::log_error("mod_vad_pcm", format!("fire asr_result failed: {error}"));
        }
        MediaBugAction::Continue
    }

    fn on_write_replace(
        &mut self,
        _ctx: &mut MediaBugContext<'_>,
        mut frame: MediaFrameMut<'_>,
    ) -> MediaBugAction {
        let mut guard = match self.state.queue.lock() {
            Ok(g) => g,
            Err(_) => return MediaBugAction::Continue, // poisoned: pass original audio through
        };
        if guard.is_empty() {
            return MediaBugAction::Continue; // idle: pass the session's own audio through
        }
        let Some(out) = frame.pcm_i16_mut() else {
            return MediaBugAction::Continue; // can't replace this frame (mis-aligned / odd length)
        };
        for slot in out.iter_mut() {
            *slot = guard.pop_front().unwrap_or(0); // underrun → silence
        }
        MediaBugAction::Continue
    }

    fn on_close(&mut self, _ctx: &mut MediaBugContext<'_>) {
        match REGISTRY.lock() {
            Ok(mut reg) => {
                reg.remove(&self.uuid);
                fswtch::log_info("mod_vad_pcm", format!("bridge closed for {}", self.uuid));
            }
            Err(_) => fswtch::log_error("mod_vad_pcm", "registry lock poisoned on close"),
        }
    }
}

// ── inbound: fswtch::play_pcm (base64 PCM) → session play queue ─────────────

fswtch::event_callback! {
    fn on_play_pcm(event) {
        let target = match event.header("Target-UUID") {
            Some(t) if !t.is_empty() => t,
            _ => {
                fswtch::log_error("mod_vad_pcm", "play_pcm event missing Target-UUID");
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
                    "mod_vad_pcm",
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
        // Look up the per-session state, then drop the registry lock before touching the queue.
        let state = match REGISTRY.lock() {
            Ok(reg) => match reg.get(&target).cloned() {
                Some(s) => s,
                None => {
                    fswtch::log_error("mod_vad_pcm", format!("no active bridge on {target}"));
                    return;
                }
            },
            Err(_) => {
                fswtch::log_error("mod_vad_pcm", "registry lock poisoned");
                return;
            }
        };
        if let Some(rate) = rate_hdr
            && rate != state.sample_rate
        {
            fswtch::log_error(
                "mod_vad_pcm",
                format!(
                    "play_pcm rate mismatch on {target}: got {rate}, want {}",
                    state.sample_rate
                ),
            );
            return;
        }
        if let Some(ch) = chan_hdr && ch != state.channels {
            fswtch::log_error(
                "mod_vad_pcm",
                format!(
                    "play_pcm channels mismatch on {target}: got {ch}, want {}",
                    state.channels
                ),
            );
            return;
        }
        let max_samples = state.sample_rate.saturating_mul(PLAY_QUEUE_MAX_SECS) as usize;
        match state.queue.lock() {
            Ok(mut q) => {
                if q.len() >= max_samples {
                    fswtch::log_error("mod_vad_pcm", format!("play queue full on {target}"));
                    return;
                }
                q.extend(samples);
            }
            Err(_) => fswtch::log_error("mod_vad_pcm", "play queue lock poisoned"),
        }
    }
}

// ── entry: attach the bridge bug to a session ────────────────────────────────

fn attach_bridge(session: Session) -> fswtch::Result<()> {
    let rate = session.read_sample_rate();
    let uuid = session.uuid().unwrap_or_default();
    let vad = Vad::new(rate as i32, 1)?;
    let state = Arc::new(BridgeState {
        queue: Mutex::new(VecDeque::new()),
        sample_rate: rate,
        channels: 1,
    });
    let config = MediaBugConfig::new(
        "rust_vad_pcm",
        "read-write-stream",
        MediaBugFlags::READ_STREAM | MediaBugFlags::WRITE_REPLACE | MediaBugFlags::NO_PAUSE,
    )?;
    let handler = PcmBridge {
        vad,
        uuid,
        state,
        seq: 0,
        mono_scratch: Vec::new(),
    };
    fswtch::attach_media_bug(session, config, handler).map(|_| {
        fswtch::log_info("mod_vad_pcm", "media bug attached");
    })
}

fswtch::app_callback! {
    fn vad_pcm_app(session, _data) {
        fswtch::log_info("mod_vad_pcm", "dialplan application invoked");
        let Some(session) = session else {
            fswtch::log_error("mod_vad_pcm", "missing session");
            return;
        };
        if let Err(error) = attach_bridge(session) {
            fswtch::log_error("mod_vad_pcm", format!("attach failed: {error}"));
        }
    }
}

fswtch::api_callback! {
    fn vad_pcm_start_api(cmd, _session, stream) {
        fswtch::log_info("mod_vad_pcm", "rust_vad_pcm_start invoked");
        let Some(stream) = stream else {
            return fswtch::FALSE;
        };
        let uuid = cmd.unwrap_or_default().trim().to_owned();
        if uuid.is_empty() {
            return stream.write("usage: rust_vad_pcm_start <uuid>\n");
        }
        let guard = match SessionGuard::locate(&uuid) {
            Ok(Some(g)) => g,
            Ok(None) => return stream.write(&format!("session not found: {uuid}\n")),
            Err(error) => return stream.write(&format!("locate failed: {error}\n")),
        };
        match guard.session() {
            Some(session) => match attach_bridge(*session) {
                Ok(()) => stream.write(&format!("bridge attached on {uuid}\n")),
                Err(error) => stream.write(&format!("attach failed: {error}\n")),
            },
            None => stream.write(&format!("session gone: {uuid}\n")),
        }
    }
}

fswtch::api_callback! {
    fn vad_pcm_stop_playback_api(cmd, _session, stream) {
        fswtch::log_info("mod_vad_pcm", "rust_vad_pcm_stop_playback invoked");
        let Some(stream) = stream else {
            return fswtch::FALSE;
        };
        let uuid = cmd.unwrap_or_default().trim().to_owned();
        if uuid.is_empty() {
            return stream.write("usage: rust_vad_pcm_stop_playback <uuid>\n");
        }
        // Look up the per-session state, then drop the registry lock before touching the queue.
        let state = match REGISTRY.lock() {
            Ok(reg) => match reg.get(&uuid).cloned() {
                Some(s) => s,
                None => return stream.write(&format!("no active bridge on {uuid}\n")),
            },
            Err(_) => return stream.write("registry lock poisoned\n"),
        };
        // Flushing the queue stops playback within one write-replace frame: the next
        // `on_write_replace` sees an empty queue and passes the session's own audio through, while
        // the VAD keeps listening for the caller's barge-in speech.
        let cleared = match state.queue.lock() {
            Ok(mut q) => {
                let n = q.len();
                q.clear();
                n
            }
            Err(_) => return stream.write("play queue lock poisoned\n"),
        };
        stream.write(&format!("cleared {cleared} queued samples on {uuid}\n"))
    }
}

fswtch::module_load! {
    fn switch_module_load(module) for "mod_vad_pcm" {
        fswtch::log_info("mod_vad_pcm", "loading module");
        module
            .application(
                fswtch::ApplicationInfo::new(
                    "rust_vad_pcm",
                    "VAD-gated bidirectional raw-PCM ESL bridge: fires fswtch::asr_result (base64 \
                     PCM) per talking frame; plays fswtch::play_pcm back into the call",
                    "Rust VAD PCM bridge",
                    "rust_vad_pcm",
                ),
                vad_pcm_app,
            )
            .and_then(|m| {
                m.api(
                    "rust_vad_pcm_start",
                    "attaches the VAD PCM bridge to an existing call by uuid",
                    "rust_vad_pcm_start <uuid>",
                    vad_pcm_start_api,
                )
            })
            .and_then(|m| {
                m.api(
                    "rust_vad_pcm_stop_playback",
                    "stops TTS playback and flushes the play buffer on a bridged call (barge-in)",
                    "rust_vad_pcm_stop_playback <uuid>",
                    vad_pcm_stop_playback_api,
                )
            })
            .inspect(|_m| {
                match fswtch::EventBinder::bind(
                    "mod_vad_pcm.play",
                    fswtch::EventType::CUSTOM,
                    Some(PLAY_SUBCLASS),
                    Some(on_play_pcm),
                    std::ptr::null_mut(),
                ) {
                    Ok(b) => std::mem::forget(b),
                    Err(e) => fswtch::log_error("mod_vad_pcm", format!("play_pcm bind failed: {e}")),
                }
            })
    }
}
