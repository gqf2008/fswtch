//! AI Agent Seat module for FreeSWITCH.
//!
//! This module registers as a FreeSWITCH **endpoint interface** named
//! `ai_agent`. Inbound calls bridge to `ai_agent/<number>` (e.g.
//! `ai_agent/1000`); FreeSWITCH then drives the call's media through this
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
pub mod event_sub;
pub mod io;
pub mod orchestrator;
pub mod runtime;
pub mod tts;
pub mod tts_ws_codec;
pub mod voice_core;

use call_core::clear_calls;

fswtch::module_exports! {
    module = ai_agent_seat,
    load = switch_module_load,
    shutdown = Some(switch_module_shutdown),
    runtime = None,
}

fn do_module_load(module: fswtch::ModuleBuilder) -> fswtch::Result<fswtch::ModuleBuilder> {
    // Bridge tracing → FreeSWITCH's `switch_log_printf` (via `fswtch::log`). This makes
    // ai-agent-seat logs appear in `freeswitch.log` / `fs_cli` like a native module's,
    // with correct levels (ERROR/WARN/INFO/DEBUG). Set `RUST_LOG=ai_agent_seat=debug`
    // to see per-frame VAD logs.
    init_tracing();

    tracing::info!("Loading ai_agent_seat module");

    // Load configuration. Resolution order (first hit wins):
    //   1. VOICE_SEAT_CONFIG env var (explicit override)
    //   2. <param name="ai_config" value="..."/> from voice_seat.conf.xml
    //      (read via FreeSWITCH's switch_xml_open_cfg — the standard FS idiom;
    //      the config is auto-bound from autoload_configs/voice_seat.conf.xml)
    //   3. ~/.local/etc/freeswitch/voice_seat.yaml (default)
    // Any failure degrades to the next; none crash the module load.
    let config_path = std::env::var("VOICE_SEAT_CONFIG").ok().unwrap_or_else(|| {
        read_ai_config_param_from_xml().unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_default();
            format!("{home}/.local/etc/freeswitch/voice_seat.yaml")
        })
    });
    if let Err(e) = config::load(&config_path) {
        tracing::warn!("Failed to load config from {}: {}", config_path, e);
    }
    // Confirm config actually loaded (tracing → stderr; visible with `freeswitch -nf`).
    if let Some(cfg) = config::get() {
        tracing::info!(
            "config loaded: pipeline={} llm_model={} llm_url={} volcano_speaker={}",
            cfg.api.pipeline_mode,
            cfg.api.llm_model,
            cfg.api.llm_url,
            cfg.api.volcano_speaker,
        );
    } else {
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
    // `io::AiAgent` (the `EndpointIoRoutines` impl).
    let io = fswtch::EndpointIoBuilder::build::<io::AiAgent>()?;

    // All-NULL state-handler table: satisfies FreeSWITCH's
    // `state_handler != NULL` assert in `switch_core_session_run` without
    // overriding the standard state handlers.
    let state_handler = fswtch::StateHandlerTable::new_null();

    // Register the endpoint interface. Inbound calls bridge to
    // `ai_agent/<number>`; FreeSWITCH routes the call's media through the
    // I/O callbacks above.
    let module = module.endpoint("ai_agent", io, state_handler)?;

    tracing::info!("ai_agent_seat module loaded successfully (endpoint: ai_agent)");
    Ok(module)
}

fswtch::module_load! {
    fn switch_module_load(module) for "ai_agent_seat" {
        do_module_load(module)
    }
}

/// Module shutdown function.
pub extern "C" fn switch_module_shutdown() -> fswtch::Status {
    tracing::info!("Shutting down ai_agent_seat module");

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

    tracing::info!("ai_agent_seat module shutdown complete");
    fswtch::SUCCESS
}

// ── tracing → FreeSWITCH switch_log_printf bridge ────────────────────────
//
// A `tracing_subscriber::Layer` that forwards each event to `fswtch::log`
// (`switch_log_printf`), so ai-agent-seat logs flow into the normal FreeSWITCH
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
        // `target` is the module path (e.g. `ai_agent_seat::io`); switch_log_printf
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

/// Read the `<param name="ai_config" value="..."/>` from the module's
/// FreeSWITCH XML config (`autoload_configs/voice_seat.conf.xml`).
///
/// Uses FreeSWITCH's `switch_xml_open_cfg` — the canonical config-read idiom,
/// safe during `switch_module_load`. Returns `None` on any failure (file not
/// bound, no `<settings>`, no `ai_config` param) so the caller falls back to
/// the default path. Never panics.
fn read_ai_config_param_from_xml() -> Option<String> {
    use fswtch::{XmlConfig, XmlNode};
    let xml = XmlConfig::open("voice_seat.conf")?;
    // `settings()` returns the <settings> node. Its CHILDREN are <param> nodes;
    // iterate them (not <settings>'s siblings — the earlier bug).
    let first_param: XmlNode<'_> = xml.settings()?.child("param")?;
    let mut node = Some(first_param);
    while let Some(p) = node {
        if p.attr("name").as_deref() == Some("ai_config") {
            return p.attr("value");
        }
        node = p.next();
    }
    None
}

fn init_tracing() {
    // Default to `info` for this crate when RUST_LOG is unset; RUST_LOG overrides
    // (e.g. `RUST_LOG=ai_agent_seat=debug` to see 50 Hz VAD decisions).
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("ai_agent_seat=info"));
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(FsLogLayer)
        .try_init();
}
