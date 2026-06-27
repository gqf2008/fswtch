//! AI Agent Seat module for FreeSWITCH.
//!
//! This module implements an AI-powered voice agent that can handle voice calls
//! with automatic speech recognition (ASR), large language model (LLM) processing,
//! and text-to-speech (TTS) synthesis.

pub mod actor;
pub mod asr;
pub mod audio_dsp;
pub mod boundary;
pub mod bug;
pub mod call_core;
pub mod config;
pub mod control;
pub mod event_sub;
pub mod llm;
pub mod tts;
pub mod tts_ws_codec;
pub mod voice_core;

use fswtch::{ApplicationInfo, Session, attach_media_bug};

use call_core::registry;
use config::get as get_config;

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

    // Start tokio runtime + actix System
    actor::start_runtime();

    // Register the voice_seat dialplan application
    let app_info = ApplicationInfo::new(
        "voice_seat",
        "AI Agent Seat Application",
        "AI Agent Seat Application",
        "voice_seat",
    );

    let module = module.application(app_info, voice_seat_app)?;

    tracing::info!("ai_agent_seat module loaded successfully");
    Ok(module)
}

fswtch::module_load! {
    fn switch_module_load(module) for "ai_agent_seat" {
        do_module_load(module)
    }
}

// FreeSWITCH application callback for voice_seat.
//
// This is called when the dialplan executes the voice_seat application.
// It attaches a media bug to intercept audio and spawns a CallActor to handle
// the AI pipeline (ASR → LLM → TTS).
fswtch::app_callback! {
    fn voice_seat_app(session, _data) {
        handle_voice_seat_app(session);
    }
}

fn handle_voice_seat_app(session: Option<Session>) {
    // Every entry from an FS thread is wrapped at the boundary: a panic here
    // must downgrade to "skip this call", never abort the FS process.
    boundary::catch_fs(|| {
        let Some(session) = session else {
            tracing::error!("voice_seat app called with null session");
            return;
        };

        // Session does not expose uuid() directly; read it from the channel.
        let uuid = session.channel().and_then(|c| c.uuid()).unwrap_or_default();
        tracing::info!("voice_seat app called for session {}", uuid);

        // Attach media bug with VAD (read/write replace so we can tap and inject).
        let bug_config = match fswtch::MediaBugConfig::new(
            "ai_agent_seat",
            "ai_agent_seat",
            fswtch::MediaBugFlags::READ_REPLACE | fswtch::MediaBugFlags::WRITE_REPLACE,
        ) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("Failed to create MediaBugConfig: {:?}", e);
                return;
            }
        };

        let bug = match bug::VoiceSeatBug::from_session(session, get_config()) {
            Ok(bug) => bug,
            Err(e) => {
                tracing::error!("Failed to create VoiceSeatBug: {}", e);
                return;
            }
        };

        match attach_media_bug(session, bug_config, bug) {
            Ok(_) => {
                tracing::info!("Media bug attached for session {}", uuid);
                // Spawn CallActor on actix System
                actor::spawn_call_actor(uuid, get_config());
            }
            Err(e) => {
                tracing::error!("Failed to attach media bug: {:?}", e);
            }
        }
    });
}

/// Module shutdown function.
pub extern "C" fn switch_module_shutdown() -> fswtch::Status {
    tracing::info!("Shutting down ai_agent_seat module");

    // Stop tokio runtime + actix System
    actor::stop_runtime();

    // Clear the registry
    registry().clear();

    tracing::info!("ai_agent_seat module shutdown complete");
    fswtch::SUCCESS
}
