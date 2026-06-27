//! CallActor - actix actor that owns the [`Orchestrator`] for one call.
//!
//! The actor is a thin adapter: it receives [`SpeechSignal`]s from the media
//! bug ([`crate::bug::VoiceSeatBug`]) and forwards them to the
//! [`Orchestrator`], which runs the actual AI pipeline (audio → LLM with
//! tool calling → TTS via the `speak` tool). TTS audio produced by the
//! orchestrator is drained from the `tts_rx` channel by a background task and
//! (eventually) written back toward the caller.
//!
//! The orchestrator is created at call-answer time (when the actor starts)
//! and torn down in `stopped`.

use actix::prelude::*;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use tokio::runtime::Runtime;
use tokio::sync::mpsc;

use crate::call_core::{
    AnswerCall, CallControl, HangupCall, SendDtmf, SpeechSignal, TransferCall, registry,
};
use crate::control::FfiControl;
use crate::orchestrator::Orchestrator;
use crate::tts::TtsSignal;
use crate::voice_core::Config;

/// Global tokio runtime for async operations (LLM HTTP, Volcano TTS WS).
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
    // Create tokio runtime for async work (LLM HTTP, TTS WebSocket).
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
/// Owns the [`Orchestrator`] and the TTS drain task for one call.
pub struct CallActorImpl {
    uuid: String,
    #[allow(dead_code)]
    config: Option<Config>,
    /// The orchestrator runs the AI pipeline. Populated in `started` (so it
    /// is created on the actix runtime, where spawned futures land correctly).
    orchestrator: Option<Arc<Orchestrator>>,
}

impl CallActorImpl {
    pub fn new(uuid: String, config: Option<Config>) -> Self {
        Self {
            uuid,
            config,
            orchestrator: None,
        }
    }
}

impl Actor for CallActorImpl {
    type Context = Context<Self>;

    fn started(&mut self, ctx: &mut Self::Context) {
        tracing::info!("CallActor started for session {}", self.uuid);

        // Create the TTS channel (orchestrator → media bug / drain task).
        let (tts_tx, tts_rx) = mpsc::channel::<TtsSignal>(crate::tts::tts_channel_capacity());

        // Build the orchestrator with the TTS producer end.
        let orchestrator = Arc::new(Orchestrator::new(
            self.uuid.clone(),
            self.config.clone(),
            tts_tx,
        ));

        // Wire the FFI call-control so `hangup`/`send_dtmf`/`transfer` tools
        // act on the live FreeSWITCH session.
        orchestrator.set_control(Arc::new(FfiControl) as Arc<dyn crate::call_core::CallControl>);

        // Eagerly connect the Volcano WS at answer time. Spawns on the actor's
        // own context so it lands on the actix arbiter (which has a tokio
        // runtime). Errors are logged inside; they do NOT poison the session.
        let orch_start = Arc::clone(&orchestrator);
        ctx.spawn(
            async move {
                if let Err(e) = orch_start.start_tts().await {
                    tracing::warn!(
                        "CallActor: orchestrator start_tts failed for {}: {e}",
                        orch_start.uuid()
                    );
                }
            }
            .into_actor(self),
        );

        // Spawn the TTS drain task on the actix arbiter (which has a tokio
        // runtime). The orchestrator pushes TtsSignal::Chunk (16 kHz i16 PCM)
        // here; this task is the sink that keeps the orchestrator's channel
        // from back-pressuring. Playback injection into the media bug is wired
        // separately; for now each chunk is logged at trace level.
        let mut tts_rx = tts_rx;
        let uuid_for_drain = self.uuid.clone();
        actix::Arbiter::current().spawn(async move {
            while let Some(signal) = tts_rx.recv().await {
                match signal {
                    TtsSignal::Chunk(audio) => {
                        tracing::trace!(
                            "TTS drain: {} samples for {}",
                            audio.len(),
                            uuid_for_drain
                        );
                    }
                    TtsSignal::ClearBuffer => {
                        // Barge-in flush — the media bug observes this signal
                        // via its own polling; this is a no-op on the drain path.
                    }
                }
            }
        });

        self.orchestrator = Some(orchestrator);
    }

    fn stopped(&mut self, _ctx: &mut Self::Context) {
        tracing::info!("CallActor stopped for session {}", self.uuid);

        if let Some(orch) = self.orchestrator.take() {
            orch.full_hangup_reset();
        }

        // Unregister from registry
        registry().unregister(&self.uuid);
    }
}

/// Handle a speech segment (completed turn) or barge-in from the media bug.
///
/// `SpeechSignal::Turn` drives the orchestrator's full pipeline (audio → LLM
/// with tools → TTS via `speak`). `SpeechSignal::BargeIn` cancels the current
/// turn. Both are spawned on the actor's own `Context` so they land on this
/// actor's arbiter and don't race the message loop.
impl Handler<SpeechSignal> for CallActorImpl {
    type Result = ();

    fn handle(&mut self, msg: SpeechSignal, ctx: &mut Self::Context) -> Self::Result {
        let Some(orch) = self.orchestrator.as_ref().map(Arc::clone) else {
            tracing::warn!(
                "CallActor: speech signal for {} but orchestrator not initialized",
                self.uuid
            );
            return;
        };
        match msg {
            SpeechSignal::Turn { audio } => {
                let uuid = self.uuid.clone();
                ctx.spawn(
                    async move {
                        let n = audio.len();
                        match orch.process_speech_segment(audio).await {
                            Some((reply, asr)) => {
                                tracing::info!(
                                    "CallActor: turn complete for {} ({} samples) \
                                     reply={} chars, asr={:?}",
                                    uuid,
                                    n,
                                    reply.chars().count(),
                                    asr
                                );
                            }
                            None => {
                                tracing::info!(
                                    "CallActor: turn discarded for {} ({} samples)",
                                    uuid,
                                    n
                                );
                            }
                        }
                    }
                    .into_actor(self),
                );
            }
            SpeechSignal::BargeIn => {
                tracing::info!("CallActor: barge-in for {}", self.uuid);
                orch.cancel_current();
            }
        }
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

        // Delegate to the FFI control plane directly (external command path).
        let control = FfiControl;
        if let Err(e) = control.send_dtmf(&self.uuid, &msg.digits) {
            tracing::warn!("SendDtmf via FfiControl failed: {e}");
        }
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

        let control = FfiControl;
        if let Err(e) = control.transfer(&self.uuid, &msg.destination) {
            tracing::warn!("TransferCall via FfiControl failed: {e}");
        }
    }
}
