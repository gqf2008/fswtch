//! Runtime lifecycle + per-call init for ai_agent_seat.
//!
//! In the Endpoint-module design there are no actix actors: the I/O callbacks
//! in [`crate::io`] own per-call state directly in a global
//! [`dashmap::DashMap`]. This module is the thin glue layer:
//! - [`start_runtime`] / [`stop_runtime`] delegate to [`crate::runtime`] to
//!   manage the process-global tokio runtime (LLM HTTP + Volcano TTS WS).
//! - [`init_call`] lazily creates a [`crate::io::CallState`] + its
//!   [`Orchestrator`](crate::orchestrator::Orchestrator) the first time the
//!   `write_frame` callback sees a session, inserts it into [`crate::io::CALLS`],
//!   and eagerly connects the Volcano WS on the tokio runtime.
//!
//! `init_call` is the replacement for the old `spawn_call_actor`: instead of
//! starting an actix actor and wiring mpsc channels to a media bug, it just
//! builds the state struct the I/O callbacks will borrow.

use std::sync::Arc;

use crate::call_core::{control, register_call};
use crate::io::{CALLS, CallState};
use crate::orchestrator::Orchestrator;

/// Start the tokio runtime (module load).
pub fn start_runtime() {
    if let Err(e) = crate::runtime::start() {
        tracing::error!("Failed to start tokio runtime: {e:?}");
    }
}

/// Stop the tokio runtime (module shutdown).
pub fn stop_runtime() {
    crate::runtime::stop();
}

/// Lazily initialize per-call state for `uuid`.
///
/// Called from `write_frame` on first frame (codec rate is known by then).
/// Builds a [`CallState`] with a fresh VAD + resampler, constructs the
/// [`Orchestrator`] (wires the shared `ai_speaking` flag + the FFI call
/// control), stores the orchestrator `Arc` in the `CallState`, and inserts
/// the entry into [`CALLS`]. Then spawns `orchestrator.start_tts()` on the
/// tokio runtime to eagerly connect the Volcano WS.
///
/// Returns an error only when the [`CallState`] could not be constructed
/// (e.g. resampler init failure). Idempotent: if a call already exists for
/// `uuid`, it logs and returns `Ok(())`.
pub fn init_call(uuid: &str, codec_rate: u32) -> anyhow::Result<()> {
    if CALLS.contains_key(uuid) {
        return Ok(());
    }

    let config = crate::config::get();
    let mut state = CallState::new(uuid.to_string(), codec_rate, config.clone())?;

    // Build the orchestrator, sharing the call's `ai_speaking` flag.
    let orch = Arc::new(Orchestrator::new(
        uuid.to_string(),
        config,
        Arc::clone(&state.ai_speaking),
    ));
    // Wire the FFI-backed call-control singleton (hangup/answer/dtmf/transfer).
    orch.set_control(control());
    let orch_for_state = Arc::clone(&orch);
    state.orchestrator = Some(orch_for_state);

    CALLS.insert(uuid.to_string(), state);
    register_call(uuid);

    tracing::info!("init_call: created call state for {uuid}");

    // Eagerly connect the Volcano WS on the tokio runtime so the first turn
    // doesn't pay the connect latency.
    let uuid_for_task = uuid.to_string();
    crate::runtime::spawn(async move {
        if let Err(e) = orch.start_tts().await {
            tracing::warn!("init_call: orchestrator start_tts failed for {uuid_for_task}: {e}");
        }
    });

    Ok(())
}
