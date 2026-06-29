//! In-process `CallControl` impl backed by the `fswtch` FFI.
//!
//! Each method locates the session by UUID via [`fswtch::SessionGuard::locate`]
//! (which read-locks the session for the guard's lifetime) and issues the
//! corresponding FreeSWITCH C call through the safe `fswtch` wrappers. The
//! underlying calls are synchronous and non-blocking, so every method here is
//! sync — no `async_trait`, no ESL socket, no round-trip.
//!
//! `fire_transcript` is the exception: it does not need a live session. It
//! builds a CUSTOM [`fswtch::Event`] with the `voice_seat::transcript` subclass,
//! stamps the `Unique-ID` header so subscribers can correlate it with the call,
//! sets the body, and fires it on the FS event bus. The fire blocks until the
//! event is actually delivered (or fails), so the caller can safely tear down
//! the orchestrator after it returns.

use anyhow::Result;
use fswtch::{Cause, Event, Session, SessionGuard};

use crate::call_core::CallControl;

/// Subclass of the CUSTOM event fired by [`FfiControl::fire_transcript`].
const TRANSCRIPT_SUBCLASS: &str = "voice_seat::transcript";

/// FFI-backed control plane.
///
/// Stateless — each operation locates the session afresh by UUID, so a single
/// `FfiControl` can be shared across calls and threads. The underlying
/// `fswtch` wrappers are `Send + Sync` and the per-op session guard is dropped
/// before the method returns.
pub struct FfiControl;

impl FfiControl {
    /// Convenience helper: locate a session by UUID and hand the read-locked
    /// [`Session`] to `f`. Returns `Err` when no such session exists (the guard
    /// could not be acquired). The guard is kept alive for the duration of `f`
    /// and dropped when `with_session` returns, releasing the read lock.
    fn with_session<R>(uuid: &str, op: &str, f: impl FnOnce(&Session) -> Result<R>) -> Result<R> {
        let guard = SessionGuard::locate(uuid)?
            .ok_or_else(|| anyhow::anyhow!("{op}: session {uuid} not located"))?;
        // `guard.session()` borrows the guard; keep the guard alive across `f`.
        let session = guard
            .session()
            .ok_or_else(|| anyhow::anyhow!("{op}: session {uuid} not located"))?;
        f(session)
    }
}

impl CallControl for FfiControl {
    fn hangup(&self, uuid: &str) -> Result<()> {
        // `Session::hangup` takes the cause by value; the guard (and its read
        // lock) lives until the end of the call so the session pointer stays
        // valid for the underlying `switch_channel_perform_hangup`.
        Self::with_session(uuid, "hangup", |session| {
            session.hangup(Cause::NORMAL_CLEARING);
            Ok(())
        })
    }

    fn answer(&self, uuid: &str) -> Result<()> {
        Self::with_session(uuid, "answer", |session| Ok(session.answer()?))
    }

    fn send_dtmf(&self, uuid: &str, digits: &str) -> Result<()> {
        // Send digits directly on the session's media path via
        // `switch_core_session_send_dtmf_string` (wrapped by `Session::send_dtmf`).
        // Faster than the `send_dtmf` dialplan app round-trip and reaches the
        // media layer even outside the dialplan execution context.
        Self::with_session(uuid, "send_dtmf", |session| {
            Ok(session.send_dtmf(digits)?)
        })
    }

    fn transfer(&self, uuid: &str, destination: &str) -> Result<()> {
        // The `transfer` dialplan application moves the channel to a new
        // extension/destination. Pass the destination verbatim as the app data.
        Self::with_session(uuid, "transfer", |session| {
            Ok(session.execute_application("transfer", destination)?)
        })
    }

    fn fire_transcript(&self, uuid: &str, body: &str) -> Result<()> {
        // Fire a CUSTOM `voice_seat::transcript` event on the FS event bus.
        // No live session is required — this uses the session-less CUSTOM path,
        // so the control plane (in the separate project) can correlate it by
        // the `Unique-ID` header even after the call has come down.
        let mut event = Event::custom(TRANSCRIPT_SUBCLASS)?;
        event.add_header("Unique-ID", uuid)?;
        event.add_body(body)?;
        event.fire()?;
        Ok(())
    }
}
