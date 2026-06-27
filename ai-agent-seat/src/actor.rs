//! CallActor - Actor-based AI pipeline for AI agent seat.
//!
//! This module implements the CallActor which runs on an actix System and handles
//! the AI pipeline: ASR → LLM → TTS.

use actix::prelude::*;
use anyhow::Result;
use std::sync::{Mutex, OnceLock};
use std::thread;
use tokio::runtime::Runtime;

use crate::call_core::{
    AnswerCall, BargeIn, CallActor, HangupCall, SendDtmf, SpeechTurn, TransferCall, registry,
};
use crate::voice_core::Config;

/// Global tokio runtime for async operations (e.g. outbound HTTP for ASR/LLM/TTS).
static RUNTIME: OnceLock<Mutex<Option<Runtime>>> = OnceLock::new();

/// Global actix System handle for actor management.
static ACTIX_SYSTEM: OnceLock<Option<System>> = OnceLock::new();

/// Returns the global tokio runtime guard storage, initializing on first access.
fn runtime_slot() -> &'static Mutex<Option<Runtime>> {
    RUNTIME.get_or_init(|| Mutex::new(None))
}

/// Start the tokio runtime and actix System.
///
/// The actix `SystemRunner` blocks, so it is driven on a dedicated background
/// thread. We store the [`System`] handle (which is `Clone + Send + Sync`) so
/// other threads can spawn actors onto the system's arbiter.
pub fn start_runtime() {
    // Create tokio runtime for async work (ASR/LLM/TTS HTTP calls).
    let runtime = Runtime::new().expect("Failed to create tokio runtime");
    *runtime_slot().lock().unwrap() = Some(runtime);

    // Drive the actix System on a dedicated thread. The SystemRunner blocks
    // until System::stop() is called (see stop_runtime).
    let init = move || {
        let runner = System::new();
        // Capture the System handle once the runner is running on this thread.
        let system = System::current();
        ACTIX_SYSTEM.get_or_init(|| Some(system));
        tracing::info!("Runtime and actix System started");
        // Block this thread until the system is stopped.
        let _ = runner.run();
    };
    thread::Builder::new()
        .name("ai-agent-seat-actix".into())
        .spawn(init)
        .expect("Failed to spawn actix system thread");
}

/// Stop the tokio runtime and actix System.
pub fn stop_runtime() {
    // Stop actix System (signals the blocking runner.run() to return).
    if let Some(Some(system)) = ACTIX_SYSTEM.get().cloned() {
        system.stop();
    }

    // Stop tokio runtime
    if let Some(runtime) = runtime_slot().lock().unwrap().take() {
        runtime.shutdown_timeout(std::time::Duration::from_secs(5));
    }

    tracing::info!("Runtime and actix System stopped");
}

/// Spawn a CallActor for a specific call.
pub fn spawn_call_actor(uuid: String, config: Option<Config>) {
    let Some(system) = ACTIX_SYSTEM.get().cloned().flatten() else {
        tracing::error!("Cannot spawn CallActor: actix System not running");
        return;
    };
    // Spawn onto the system's arbiter. Actor::start() must run within an actix
    // runtime context, which arbiter().spawn() provides.
    system.arbiter().spawn(async move {
        let actor = CallActorImpl::new(uuid.clone(), config);
        let addr = actor.start();

        // Register in registry
        registry().register(uuid.clone(), addr);

        tracing::info!("CallActor spawned for session {}", uuid);
    });
}

/// CallActor implementation.
///
/// Handles the AI pipeline: ASR → LLM → TTS.
pub struct CallActorImpl {
    uuid: String,
    #[allow(dead_code)]
    config: Option<Config>,
    /// Conversation history for LLM context.
    conversation: Vec<ConversationMessage>,
}

/// Conversation message for LLM context.
#[derive(Clone)]
pub struct ConversationMessage {
    pub role: String, // "user" or "assistant"
    pub content: String,
}

impl CallActorImpl {
    pub fn new(uuid: String, config: Option<Config>) -> Self {
        Self {
            uuid,
            config,
            conversation: Vec::new(),
        }
    }

    /// Process a speech turn through ASR and LLM.
    #[allow(dead_code)]
    async fn process_speech_turn(&mut self, audio: Vec<i16>) -> Result<String> {
        // TODO: Implement ASR processing
        // For now, return empty string
        tracing::info!("Processing speech turn ({} samples)", audio.len());

        // Placeholder: return empty response
        Ok("I heard you speaking.".to_string())
    }

    /// Generate TTS audio from text.
    #[allow(dead_code)]
    async fn generate_tts(&self, text: &str) -> Result<Vec<i16>> {
        // TODO: Implement TTS generation
        // For now, return empty audio
        tracing::info!("Generating TTS for: {}", text);

        // Placeholder: return empty audio
        Ok(Vec::new())
    }
}

impl Actor for CallActorImpl {
    type Context = Context<Self>;

    fn started(&mut self, _ctx: &mut Self::Context) {
        tracing::info!("CallActor started for session {}", self.uuid);
    }

    fn stopped(&mut self, _ctx: &mut Self::Context) {
        tracing::info!("CallActor stopped for session {}", self.uuid);

        // Unregister from registry
        registry().unregister(&self.uuid);
    }
}

/// Handle SpeechTurn message.
///
/// Spawns the ASR → LLM → TTS pipeline on the actor's own `Context` via
/// [`AsyncContext::spawn`] (rather than `actix::spawn`, which would land on an
/// arbitrary arbiter and race the actor's message loop). `into_actor(self)`
/// pins the future to this actor's `LocalSet` and gives it `&mut self` access.
impl Handler<SpeechTurn> for CallActorImpl {
    type Result = ();

    fn handle(&mut self, msg: SpeechTurn, ctx: &mut Self::Context) -> Self::Result {
        let uuid = self.uuid.clone();
        let audio = msg.audio;

        ctx.spawn(
            async move {
                // TODO: Implement full ASR → LLM → TTS pipeline, dispatching TTS
                // audio back to the FreeSWITCH media bug via the registry / a
                // TTS channel.
                tracing::info!(
                    "Processing speech turn for session {} ({} samples)",
                    uuid,
                    audio.len()
                );
            }
            .into_actor(self),
        );
    }
}

/// Handle BargeIn message.
impl Handler<BargeIn> for CallActorImpl {
    type Result = ();

    fn handle(&mut self, _msg: BargeIn, _ctx: &mut Self::Context) -> Self::Result {
        tracing::info!("Barge-in detected for session {}", self.uuid);

        // Clear conversation history or interrupt current TTS
        // For now, just log
    }
}

/// Handle AnswerCall message.
impl Handler<AnswerCall> for CallActorImpl {
    type Result = ();

    fn handle(&mut self, _msg: AnswerCall, _ctx: &mut Self::Context) -> Self::Result {
        tracing::info!("Call answered for session {}", self.uuid);
    }
}

/// Handle HangupCall message.
impl Handler<HangupCall> for CallActorImpl {
    type Result = ();

    fn handle(&mut self, msg: HangupCall, ctx: &mut Self::Context) -> Self::Result {
        tracing::info!(
            "Hangup requested for session {}: {:?}",
            self.uuid,
            msg.cause
        );

        // Stop the actor
        ctx.stop();
    }
}

/// Handle SendDtmf message.
impl Handler<SendDtmf> for CallActorImpl {
    type Result = ();

    fn handle(&mut self, msg: SendDtmf, _ctx: &mut Self::Context) -> Self::Result {
        tracing::info!("DTMF requested for session {}: {}", self.uuid, msg.digits);

        // TODO: Implement DTMF sending via fswtch FFI
    }
}

/// Handle TransferCall message.
impl Handler<TransferCall> for CallActorImpl {
    type Result = ();

    fn handle(&mut self, msg: TransferCall, _ctx: &mut Self::Context) -> Self::Result {
        tracing::info!(
            "Transfer requested for session {} to {}",
            self.uuid,
            msg.destination
        );

        // TODO: Implement call transfer via fswtch FFI
    }
}

/// Implement CallActor trait for CallActorImpl.
impl CallActor for CallActorImpl {
    fn uuid(&self) -> &str {
        &self.uuid
    }

    fn process_audio(&mut self, _audio: &[i16], _sample_rate: u32) -> bool {
        // Caller audio is processed through VAD by `VoiceSeatBug`; the actor
        // receives completed speech turns via `SpeechTurn`. Continue processing.
        true
    }

    fn write_tts_audio(&mut self, _audio: &[i16], _sample_rate: u32) -> Result<()> {
        // TODO: Write TTS audio back toward the FreeSWITCH media bug.
        Ok(())
    }

    fn handle_speech_turn(&mut self, text: String) -> Result<()> {
        // Add to conversation history
        self.conversation.push(ConversationMessage {
            role: "user".to_string(),
            content: text,
        });

        Ok(())
    }

    fn handle_barge_in(&mut self) -> Result<()> {
        tracing::info!("Barge-in handled for session {}", self.uuid);
        Ok(())
    }
}
