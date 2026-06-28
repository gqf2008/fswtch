//! AI Agent Seat module for FreeSWITCH.
//!
//! This module registers as a FreeSWITCH **endpoint interface** named
//! `ai_agent`. Inbound calls bridge to `ai_agent/<number>` (e.g.
//! `ai_agent/1000`); FreeSWITCH then drives the call's media through this
//! module's [`IoRoutines`](fswtch::IoRoutinesBuilder) table: the
//! `write_frame` / `read_frame` / `kill_channel` callbacks in [`io`] run on
//! the media thread at 50 Hz (20 ms frames).
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
    // Initialize tracing subscriber for logging
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    tracing::info!("Loading ai_agent_seat module");

    // Load configuration from voice_seat.conf.xml path
    if let Ok(config_path) = std::env::var("VOICE_SEAT_CONFIG")
        && let Err(e) = config::load(&config_path)
    {
        tracing::warn!("Failed to load config from {}: {}", config_path, e);
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

    // Stop tokio runtime.
    actor::stop_runtime();

    // Clear the live-call registry + per-call state.
    clear_calls();
    io::CALLS.clear();

    tracing::info!("ai_agent_seat module shutdown complete");
    fswtch::SUCCESS
}
