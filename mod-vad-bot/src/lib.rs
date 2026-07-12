//! AI Agent Seat module for FreeSWITCH.
//!
//! This module registers as a FreeSWITCH **endpoint interface** named
//! `fswtch_vad_bot`. Inbound calls bridge to `fswtch_vad_bot/<number>` (e.g.
//! `fswtch_vad_bot/1000`); FreeSWITCH then drives the call's media through this
//! module's [`IoRoutines`](fswtch::IoRoutinesBuilder) table: the
//! `write_frame` / `read_frame` / `kill_channel` callbacks in [`io`] run on
//! the media thread at 50 Hz (20 ms frames).
//!
//! # Global allocator
//!
//! [`mimalloc`] is installed as the process-wide allocator (see
//! `#[global_allocator]` below). The media thread does small, frequent
//! allocations (per-frame `Vec`s in the VAD bypass), and mimalloc's
//! thread-local caches are friendlier to that pattern than the system
//! allocator, reducing jitter on the 20 ms real-time budget.
//!
//! Pipeline (audio-native LLM, no separate ASR): the caller's audio (arriving
//! in `write_frame`) is run through VAD; when a speech segment completes, an
//! orchestrator turn is spawned on the tokio runtime. The orchestrator encodes
//! the audio as a WAV data URI, sends it to the LLM as a multimodal user
//! message, and synthesizes the LLM's `speak(text)` tool call via Volcano TTS.
//! The resulting 16 kHz i16 PCM is pushed into [`io::CallState::tts_accum`],
//! which `read_frame` drains toward the caller.
//!
//! Per-call state ([`io::CallState`]) lives in a global
//! [`dashmap::DashMap`]([`io::CALLS`]) keyed by session UUID, because the I/O
//! callbacks receive no `user_data` parameter.

// Process-wide allocator: mimalloc. Installed before any other item so it
// takes effect for all subsequent allocations (incl. in dependencies). The
// media thread's per-frame small allocations benefit from mimalloc's
// thread-local caches — less jitter on the 20 ms real-time budget than the
// system allocator.
use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

pub mod actor;
pub mod audio_dsp;
pub mod boundary;
pub mod call_core;
pub mod config;
pub mod control;
pub mod doubao_responses;
pub mod event_sub;
pub mod io;
pub mod mimo_tts;
pub mod orchestrator;
pub mod providers;
pub mod runtime;
pub mod tts;
pub mod tts_ws_codec;
pub mod voice_core;

use call_core::clear_calls;

fswtch::module_exports! {
    module = mod_vad_bot,
    load = switch_module_load,
    shutdown = Some(switch_module_shutdown),
    runtime = None,
}

fn do_module_load(module: fswtch::ModuleBuilder) -> fswtch::Result<fswtch::ModuleBuilder> {
    // Bridge tracing → FreeSWITCH's `switch_log_printf` (via `fswtch::log`). This makes
    // mod-vad-bot logs appear in `freeswitch.log` / `fs_cli` like a native module's,
    // with correct levels (ERROR/WARN/INFO/DEBUG). Set `RUST_LOG=mod_vad_bot=debug`
    // to see per-frame VAD logs.
    init_tracing();

    tracing::info!("Loading mod_vad_bot module");

    // Load configuration from FreeSWITCH native XML config
    // (autoload_configs/mod-vad-bot.conf.xml → <settings><param .../></settings>).
    // Each <param name="..."> maps to a Config field. Channel variables
    // (VOICE_SEAT_*) set in the dialplan override these per-call at
    // outgoing_channel time.
    match load_config_from_xml() {
        Ok(cfg) => {
            tracing::info!(
                "config loaded from XML: pipeline={} llm_model={} llm_base_url={} tts_provider={}",
                cfg.api.pipeline_mode,
                cfg.api.llm_model,
                cfg.api.llm_base_url,
                cfg.api.tts_provider,
            );
            config::store(cfg);
        }
        Err(e) => {
            tracing::warn!("Failed to load config from XML: {e}");
        }
    }
    if config::get().is_none() {
        tracing::warn!("no config loaded — orchestrator will use canned responses");
    }

    // Start tokio runtime (LLM HTTP + Volcano TTS WebSocket).
    actor::start_runtime();

    // Bind to CUSTOM voice_seat::command events (external control plane).
    if let Err(e) = event_sub::bind() {
        tracing::warn!("event_sub::bind failed (continuing): {:?}", e);
    }

    // Build the I/O routines table: read_frame (drain TTS), write_frame
    // (VAD + spawn orchestrator), kill_channel (teardown), outgoing_channel
    // (create the B leg). fswtch's generic trampolines dispatch to
    // `io::VadBot` (the `EndpointIoRoutines` impl).
    let io = fswtch::EndpointIoBuilder::build::<io::VadBot>()?;

    // All-NULL state-handler table: satisfies FreeSWITCH's
    // `state_handler != NULL` assert in `switch_core_session_run` without
    // overriding the standard state handlers.
    let state_handler = fswtch::StateHandlerTable::new_null();

    // Register the endpoint interface. Inbound calls bridge to
    // `fswtch_vad_bot/<number>`; FreeSWITCH routes the call's media through the
    // I/O callbacks above.
    let module = module.endpoint("fswtch_vad_bot", io, state_handler)?;

    tracing::info!("mod_vad_bot module loaded successfully (endpoint: fswtch_vad_bot)");
    Ok(module)
}

fswtch::module_load! {
    fn switch_module_load(module) for "mod_vad_bot" {
        do_module_load(module)
    }
}

/// Module shutdown function.
pub extern "C" fn switch_module_shutdown() -> fswtch::Status {
    tracing::info!("Shutting down mod_vad_bot module");

    // Unbind events first so an in-flight callback can't enter unloaded code.
    event_sub::unbind();

    // Drop per-call state WHILE the tokio runtime is still alive. Each
    // CallState drop → actor.kill() → CallActor drop → SessionInner::Drop,
    // and SessionInner::Drop spawns a task to send `finish_session` /
    // `finish_connection` / `Shutdown` over the WS and await driver_loop exit.
    // That spawned task needs the runtime to run. If we stopped the runtime
    // first (the old order), those tasks would be aborted and the WS close
    // frames never sent — connections left half-open until the server's idle
    // timeout. Clearing first lets the graceful-close tasks run to completion
    // during `stop_runtime`'s shutdown_timeout.
    clear_calls();
    io::CALLS.clear();

    // Stop tokio runtime last — drains the Drop tasks spawned above.
    actor::stop_runtime();

    tracing::info!("mod_vad_bot module shutdown complete");
    fswtch::SUCCESS
}

// ── tracing → FreeSWITCH switch_log_printf bridge ────────────────────────
//
// A `tracing_subscriber::Layer` that forwards each event to `fswtch::log`
// (`switch_log_printf`), so mod-vad-bot logs flow into the normal FreeSWITCH
// log system (visible via `fs_cli` / `freeswitch.log`) instead of stderr.

use std::fmt::Write as _;
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::Context as LayerContext;
use tracing_subscriber::{Layer, layer::SubscriberExt, util::SubscriberInitExt};

struct FsLogLayer;

impl<S: tracing::Subscriber> Layer<S> for FsLogLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: LayerContext<'_, S>) {
        let mut visitor = FieldCollector::default();
        event.record(&mut visitor);

        let meta = event.metadata();
        let level = match *meta.level() {
            tracing::Level::ERROR => fswtch::LogLevel::Error,
            tracing::Level::WARN => fswtch::LogLevel::Warning,
            tracing::Level::INFO => fswtch::LogLevel::Info,
            tracing::Level::DEBUG => fswtch::LogLevel::Debug,
            tracing::Level::TRACE => fswtch::LogLevel::Debug2,
        };
        // `target` is the module path (e.g. `mod_vad_bot::io`); switch_log_printf
        // surfaces it as the log line's tag.
        fswtch::log(meta.target(), level, visitor.0);
    }
}

/// Visits a tracing event's fields, formatting them as `message field=value …`.
#[derive(Default)]
struct FieldCollector(String);

impl Visit for FieldCollector {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if !self.0.is_empty() {
            self.0.push(' ');
        }
        if field.name() == "message" {
            // `format_args!` Debug renders as the formatted text (no quotes).
            let _ = write!(self.0, "{:?}", value);
        } else {
            let _ = write!(self.0, "{}={:?}", field.name(), value);
        }
    }
}

/// Load the AI [`Config`] from the module's FreeSWITCH XML config
/// (`autoload_configs/mod-vad-bot.conf.xml`).
///
/// Iterates all `<param name="..." value="..."/>` children of `<settings>`
/// and maps each `name` to a `Config` field. Uses FreeSWITCH's
/// `switch_xml_open_cfg` (the canonical config-read idiom, safe during
/// `switch_module_load`).
///
/// `system_prompt_file` is a special param: its value is a file path whose
/// contents are read and stored as `Config::system_prompt`.
fn load_config_from_xml() -> anyhow::Result<crate::voice_core::Config> {
    use crate::voice_core::Config;
    use fswtch::{XmlConfig, XmlNode};

    // `XmlConfig::open` returns the <configuration> root node via
    // `switch_xml_open_cfg`. `settings()` returns that same root (the fswtch
    // binding name is misleading — FreeSWITCH's `switch_xml_open_cfg` writes
    // the root node, not a <settings> child). So we navigate:
    //   <configuration> → child <settings> → children <param>.
    let xml = XmlConfig::open("mod-vad-bot.conf")
        .ok_or_else(|| anyhow::anyhow!("mod-vad-bot.conf not bound or missing"))?;
    let root = xml
        .settings()
        .ok_or_else(|| anyhow::anyhow!("XmlConfig returned no root node"))?;

    // <configuration> → <settings>
    let settings = root
        .child("settings")
        .ok_or_else(|| anyhow::anyhow!("no <settings> node under <configuration>"))?;

    let first_param: XmlNode<'_> = settings
        .child("param")
        .ok_or_else(|| anyhow::anyhow!("no <param> nodes in <settings>"))?;

    let mut cfg = Config::default();
    let mut node = Some(first_param);
    while let Some(p) = node {
        let (name, value) = match (p.attr("name"), p.attr("value")) {
            (Some(n), Some(v)) => (n, v),
            _ => {
                node = p.next();
                continue;
            }
        };
        apply_param(&mut cfg, &name, &value);
        node = p.next();
    }
    Ok(cfg)
}

/// Apply one `<param name="..." value="...">` to the [`Config`].
fn apply_param(cfg: &mut crate::voice_core::Config, name: &str, value: &str) {
    let api = &mut cfg.api;
    match name {
        // ── API / LLM ──
        "pipeline_mode" => api.pipeline_mode = value.to_string(),
        "llm_base_url" => api.llm_base_url = value.to_string(),
        "llm_key" => api.llm_key = value.to_string(),
        "llm_model" => api.llm_model = value.to_string(),
        "llm_temperature" => api.llm_temperature = value.parse().ok(),
        "llm_max_tokens" => api.llm_max_tokens = value.parse().ok(),
        "llm_stream" => api.llm_stream = value == "true" || value == "1",
        "llm_auth_mode" => api.llm_auth_mode = value.to_string(),
        // ── TTS provider selection ──
        "tts_provider" => api.tts_provider = value.to_string(),
        // ── Volcano TTS ──
        "volcano_api_key" => api.volcano_api_key = value.to_string(),
        "volcano_resource_id" => api.volcano_resource_id = value.to_string(),
        "volcano_speaker" => api.volcano_speaker = value.to_string(),
        "volcano_tts_url" => api.volcano_tts_url = value.to_string(),
        "volcano_tts_sample_rate" => { /* accepted for compat; pipeline forces 8kHz */ }
        // ── MIMO TTS ──
        "mimo_tts_voice" => api.mimo_tts_voice = value.to_string(),
        "mimo_tts_format" => api.mimo_tts_format = value.to_string(),
        // ── ASR ──
        "asr_model" => api.asr_model = value.to_string(),
        // ── system prompt (from file) ──
        "system_prompt_file" => match std::fs::read_to_string(value) {
            Ok(content) => cfg.system_prompt = Some(content),
            Err(e) => tracing::warn!("system_prompt_file {} read failed: {e}", value),
        },
        "system_prompt" => cfg.system_prompt = Some(value.to_string()),
        // ── VAD ──
        "vad_speech_threshold" => cfg.vad.speech_threshold = value.parse().unwrap_or(0.5),
        "vad_silence_timeout_ms" => cfg.vad.silence_timeout_ms = value.parse().unwrap_or(500),
        "vad_min_speech_rms" => cfg.vad.min_speech_rms = value.parse().unwrap_or(0.01),
        "vad_barge_in_confirm_ms" => cfg.vad.barge_in_confirm_ms = value.parse().unwrap_or(80),
        "vad_speech_onset_ms" => cfg.vad.speech_onset_ms = value.parse().unwrap_or(80.0),
        "vad_speech_onset_decay" => cfg.vad.speech_onset_decay = value.parse().unwrap_or(0.25),
        // ── Audio ──
        "audio_fade_out_ms" => cfg.audio.fade_out_ms = value.parse().unwrap_or(80),
        // ── max call duration ──
        "max_call_secs" => cfg.max_call_secs = value.parse().unwrap_or(0),
        _ => tracing::debug!("unknown config param: {name}={value}"),
    }
}

fn init_tracing() {
    // Default to `info` for this crate when RUST_LOG is unset; RUST_LOG overrides
    // (e.g. `RUST_LOG=mod_vad_bot=debug` to see 50 Hz VAD decisions).
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("mod_vad_bot=info"));
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(FsLogLayer)
        .try_init();
}
