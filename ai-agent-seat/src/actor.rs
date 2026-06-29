//! Per-call actor (kameo) — owns the async pipeline state for one call.
//!
//! Each call spawns a [`CallActor`] at `outgoing_channel` time; the actor runs
//! on the module tokio runtime and owns the conversation history + Volcano TTS
//! session. The sync media thread (`io::write_frame`) feeds it speech segments
//! via a per-call mpsc channel that the actor consumes with
//! [`ActorRef::attach_stream`] as `StreamMessage<Vec<i16>>` — a non-blocking
//! `try_send` on the media thread, zero `spawn` per segment. On hangup,
//! `kill_channel` calls `actor_ref.kill()` which **immediately aborts** the
//! actor task (any in-flight turn) and drops its state — the TTS WebSocket is
//! torn down via `SessionInner::Drop`, fixing the "TTS keeps running after
//! hangup" leak of the old spawn+Arc model.
//!
//! TTS audio flows through an SPSC ringbuf (`ringbuf::HeapRb<i16>`): the TTS
//! driver loop owns the `Producer` (via the `on_audio` callback handed to the
//! Volcano session) and pushes PCM directly; `io::read_frame` owns the
//! `Consumer` and drains it. No forwarder task. Barge-in is a `BargeIn`
//! message that cancels the current turn's `CancellationToken` and flushes
//! the consumer.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use kameo::Actor;
use kameo::actor::{ActorRef, Spawn, WeakActorRef};
use kameo::error::{ActorStopReason, PanicError};
use kameo::message::{Context, Message, StreamMessage};
use parking_lot::Mutex;
// `Producer` trait is required for `ringbuf::Prod::push_slice` method
// resolution (it is a trait method, not inherent). We use the `Direct`
// wrappers (`Prod`/`Cons`) on an `Arc<HeapRb<i16>>` directly (no `split`),
// so only `Producer` needs to be in scope here.
use ringbuf::traits::Producer;
use tokio_util::sync::CancellationToken;

use ringbuf::traits::Consumer;

use crate::call_core::{CallControl, control, register_call};
use crate::io::{CALLS, CallState};
use crate::orchestrator::{ChatMessage, MAX_HISTORY, turn_pipeline};
use crate::tts::{OnAudio, VolcanoBidirectionalSession};
use crate::voice_core::Config;

/// Barge-in: cancel the in-flight turn + flush the TTS ringbuf.
pub struct BargeIn;

/// A finished turn's history entries, sent back from the background turn task
/// so the actor (single-threaded mailbox) is the sole writer of conversation
/// history.
pub struct TurnDone {
    pub user: Option<ChatMessage>,
    pub assistant: Option<ChatMessage>,
}

/// One actor per call. Owns conversation + TTS session; its task lifetime is
/// the call's async lifetime.
pub struct CallActor {
    uuid: String,
    config: Option<Config>,
    conversation: Mutex<Vec<ChatMessage>>,
    tts_session: Option<VolcanoBidirectionalSession>,
    turn_cancel: CancellationToken,
    ai_speaking: Arc<AtomicBool>,
    control: Arc<dyn CallControl>,
    /// Receiver half of the per-call speech-segment channel. `take`n in
    /// `on_start` and attached via `attach_stream` so the actor mailbox
    /// consumes `StreamMessage<Vec<i16>>` items. `None` after `on_start`.
    speech_rx: Option<tokio::sync::mpsc::Receiver<Vec<i16>>>,
    /// Self-reference, stored in `on_start` so background turn tasks can
    /// `tell(TurnDone)` back. `None` only before `on_start` runs.
    actor_ref: Option<ActorRef<Self>>,
}

impl CallActor {
    pub fn new(
        uuid: String,
        config: Option<Config>,
        ai_speaking: Arc<AtomicBool>,
        control: Arc<dyn CallControl>,
        on_audio: OnAudio,
        speech_rx: tokio::sync::mpsc::Receiver<Vec<i16>>,
    ) -> Self {
        let tts_session = config
            .as_ref()
            .filter(|c| !c.api.volcano_api_key.is_empty())
            .map(|c| {
                VolcanoBidirectionalSession::new(
                    c.api.volcano_api_key.clone(),
                    c.api.volcano_resource_id.clone(),
                    c.api.volcano_speaker.clone(),
                    uuid.clone(),
                    on_audio,
                )
            });
        let mut conversation = Vec::new();
        if let Some(cfg) = config.as_ref()
            && let Some(prompt) = cfg.system_prompt.as_ref()
            && !prompt.is_empty()
        {
            conversation.push(ChatMessage::text("system", prompt.clone()));
        }
        Self {
            uuid,
            config,
            conversation: Mutex::new(conversation),
            tts_session,
            turn_cancel: CancellationToken::new(),
            ai_speaking,
            control,
            speech_rx: Some(speech_rx),
            actor_ref: None,
        }
    }

    fn push_message(&self, msg: ChatMessage) {
        let mut c = self.conversation.lock();
        c.push(msg);
        if c.len() > MAX_HISTORY {
            let drop_n = c.len() - MAX_HISTORY;
            c.drain(..drop_n);
        }
    }
}

impl Actor for CallActor {
    type Args = Self;
    type Error = PanicError;

    async fn on_start(
        mut state: Self::Args,
        actor_ref: ActorRef<Self>,
    ) -> Result<Self, Self::Error> {
        state.actor_ref = Some(actor_ref.clone());
        // Attach the per-call speech-segment channel as a stream. The media
        // thread does a non-blocking `try_send` at end-of-speech; here we
        // consume it through the actor mailbox as `StreamMessage<Vec<i16>>`
        // — zero spawn per segment (replaces the old `runtime::spawn(tell)`).
        if let Some(rx) = state.speech_rx.take() {
            let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
            actor_ref.attach_stream(stream, (), ());
        }
        // Eagerly connect the Volcano WS so the first turn isn't delayed by the
        // ~3 s handshake. Lazy-retry on `synthesize` if this fails.
        if let Some(session) = &state.tts_session {
            if let Err(e) = session.start().await {
                tracing::warn!(
                    "CallActor {}: TTS eager connect failed (will lazy-retry): {e}",
                    state.uuid
                );
            } else {
                tracing::info!("CallActor {}: TTS connected at init", state.uuid);
            }
        }
        Ok(state)
    }

    async fn on_stop(
        &mut self,
        _actor_ref: WeakActorRef<Self>,
        reason: ActorStopReason,
    ) -> Result<(), Self::Error> {
        // Kill any in-flight turn, then let actor state drop (which drops
        // `tts_session` → `SessionInner::Drop` sends WS Shutdown).
        self.turn_cancel.cancel();
        tracing::info!("CallActor stopped for {} ({:?})", self.uuid, reason);
        Ok(())
    }
}

impl Message<StreamMessage<Vec<i16>, (), ()>> for CallActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: StreamMessage<Vec<i16>, (), ()>,
        _ctx: &mut Context<Self, Self::Reply>,
    ) {
        // Only act on actual speech segments; ignore Started/Finished signals.
        let StreamMessage::Next(audio) = msg else {
            return;
        };
        if audio.is_empty() {
            return;
        }
        // Clear any leftover TTS audio from the previous turn before starting
        // a new one — prevents audio overlap (old turn's TTS tail + new turn's
        // TTS head mixing in the ringbuf).
        if let Some(mut st) = crate::io::CALLS.get_mut(&self.uuid) {
            let st = st.value_mut();
            if let Some(cons) = st.tts_cons.as_mut() {
                cons.clear();
            }
        }
        tracing::info!(
            "CallActor {}: speech segment received ({} samples), starting turn",
            self.uuid,
            audio.len()
        );
        // Run the turn IN-LINE (mailbox-sequential): the next SpeechSegment or
        // BargeIn queues in the mailbox until this turn finishes. This prevents
        // the "each new segment cancels the previous LLM call" storm that
        // happens when VAD splits a user's utterance into multiple segments
        // (every >500ms pause). Turns are strictly ordered — no concurrent TTS
        // mixing, no cancelled-LLM waste. Barge-in latency = current turn's
        // completion (acceptable for phone AI; instant barge-in needs
        // pre-emptive mailbox interruption, a future enhancement).
        let cancel = self.turn_cancel.clone();
        let tts = self.tts_session.clone();
        let conv_snapshot = self.conversation.lock().clone();
        let config = self.config.clone();
        let uuid = self.uuid.clone();
        let ai_speaking = Arc::clone(&self.ai_speaking);
        let control = Arc::clone(&self.control);
        let Some(actor_ref) = self.actor_ref.clone() else {
            tracing::error!("CallActor {}: no actor_ref (on_start not run?)", uuid);
            return;
        };
        turn_pipeline(
            uuid,
            config,
            conv_snapshot,
            tts,
            audio,
            cancel,
            ai_speaking,
            control,
            actor_ref,
        )
        .await;
    }
}

impl Message<BargeIn> for CallActor {
    type Reply = ();

    async fn handle(&mut self, _msg: BargeIn, _ctx: &mut Context<Self, Self::Reply>) {
        self.turn_cancel.cancel();
        self.ai_speaking.store(false, Ordering::Relaxed);
        if let Some(mut s) = CALLS.get_mut(&self.uuid) {
            s.value_mut().clear_tts();
        }
        tracing::info!("CallActor {}: barge-in (turn cancelled)", self.uuid);
    }
}

impl Message<TurnDone> for CallActor {
    type Reply = ();

    async fn handle(&mut self, msg: TurnDone, _ctx: &mut Context<Self, Self::Reply>) {
        if let Some(u) = msg.user {
            self.push_message(u);
        }
        if let Some(a) = msg.assistant {
            self.push_message(a);
        }
    }
}

// ── Module entry points (kept compatible with lib.rs) ───────────────────

pub fn start_runtime() {
    let _ = crate::runtime::start();
}

pub fn stop_runtime() {
    crate::runtime::stop();
}

/// Initialize per-call state + spawn the CallActor.
///
/// Idempotent: a no-op if `uuid` is already in [`CALLS`].
pub fn init_call(uuid: &str, codec_rate: u32) -> Result<()> {
    // Serialize init: `outgoing_channel` (originate thread) and the first
    // `write_frame` (media thread) can both call init_call for the same UUID
    // at ~the same instant. A `contains_key` + `insert` pair is a TOCTOU race
    // that spawns two CallActors + two TTS WebSockets (one orphaned, leaked).
    // The init lock makes the check-and-insert atomic. Held only during init
    // (fast relative to call lifetime); CallState lookups use CALLS directly.
    use std::sync::Mutex as StdMutex;
    static INIT_LOCK: std::sync::LazyLock<StdMutex<()>> =
        std::sync::LazyLock::new(|| StdMutex::new(()));
    // Ignore poison: if a prior init panicked mid-init, the guard is still
    // usable (we only need mutual exclusion, not lock-internal invariants).
    // Using `.unwrap()` would poison-chain every subsequent call — one bad
    // init would break ALL future calls, not just the one that panicked.
    let _guard = INIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    if CALLS.contains_key(uuid) {
        return Ok(());
    }
    let config = crate::config::get();
    let mut state = CallState::new(uuid.to_string(), codec_rate, config.clone())?;
    // Share the AI-speaking flag between CallState (media thread) + CallActor.
    let ai_speaking = Arc::clone(&state.ai_speaking);

    // ── Media plumbing ────────────────────────────────────────────────
    // SPSC ringbuf for TTS PCM: the TTS driver loop owns the `Producer`
    // (via `on_audio`), `read_frame` owns the `Consumer` (stored in
    // CallState). 160000 samples = 20 s at 8 kHz headroom — must be large
    // enough for the longest TTS sentence (5-10s) plus buffering margin.
    // `push_slice` silently drops samples on overflow, so undersizing causes
    // audio gaps ("漏音").
    //
    // We use the `Direct` wrappers (`ringbuf::Prod` / `ringbuf::Cons`) on an
    // `Arc<HeapRb<i16>>` rather than `HeapRb::split()` (which yields the
    // `Caching` wrappers). `Direct` is `Send + Sync` — required because
    // `CallState` lives in a global `DashMap` `static` — while `Caching`
    // uses `Cell` caches and is `!Sync`. The caches are unnecessary for our
    // SPSC pattern.
    let rb = std::sync::Arc::new(ringbuf::HeapRb::<i16>::new(160000));
    let mut tts_prod = ringbuf::Prod::new(rb.clone());
    let tts_cons = ringbuf::Cons::new(rb);
    // `on_audio` is handed to the Volcano session; the driver loop calls it
    // with each PCM chunk, pushing directly into the ringbuf — no forwarder.
    let on_audio: OnAudio = Box::new(move |chunk| {
        tts_prod.push_slice(chunk);
    });

    // Per-call speech-segment channel: `write_frame` `try_send`s here at
    // end-of-speech; the CallActor consumes it via `attach_stream`. Replaces
    // the old per-segment `runtime::spawn(tell)`.
    let (speech_tx, speech_rx) = tokio::sync::mpsc::channel::<Vec<i16>>(8);

    // Wire the consumer + sender into CallState before it lands in CALLS.
    state.tts_cons = Some(tts_cons);
    state.speech_tx = Some(speech_tx);

    let actor = CallActor::new(
        uuid.to_string(),
        config,
        ai_speaking,
        control(),
        on_audio,
        speech_rx,
    );
    // `CallActor::spawn` internally `tokio::spawn`s the actor task, which
    // requires a tokio runtime context. `init_call` runs on the FS media
    // thread (no runtime), so enter the module runtime's context first.
    let actor_ref = match crate::runtime::handle() {
        Some(handle) => {
            let _guard = handle.enter();
            CallActor::spawn(actor)
        }
        None => {
            return Err(anyhow::anyhow!(
                "no tokio runtime (start_runtime not called?)"
            ));
        }
    };
    state.actor = Some(actor_ref);
    CALLS.insert(uuid.to_string(), state);
    register_call(uuid);
    Ok(())
}
