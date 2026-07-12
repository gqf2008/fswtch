//! Call-control trait + FFI-backed control plane for mod_vad_bot.
//!
//! In the Endpoint-module design there are no actix actors and no message
//! types: the I/O callbacks in [`crate::io`] own per-call state directly in a
//! global [`dashmap::DashMap`]. This module keeps only the [`CallControl`]
//! trait (used by the orchestrator to hangup / answer / send DTMF / transfer /
//! fire transcript events) and a thin UUID → exists registry so the event
//! subscription layer can tell which calls are still live.

use anyhow::Result;
use dashmap::DashMap;
use std::sync::Arc;

/// Call control trait for controlling an active call.
///
/// All methods are synchronous: the FFI-backed implementation (see
/// `crate::control::FfiControl`) issues FreeSWITCH C calls that are themselves
/// non-blocking, so there is no need for `async_trait`. This also keeps the
/// trait `dyn`-compatible should a caller want to store it as a trait object.
pub trait CallControl: Send + Sync {
    fn hangup(&self, uuid: &str) -> Result<()>;
    fn answer(&self, uuid: &str) -> Result<()>;
    fn send_dtmf(&self, uuid: &str, digits: &str) -> Result<()>;
    fn transfer(&self, uuid: &str, destination: &str) -> Result<()>;
    fn fire_transcript(&self, uuid: &str, body: &str) -> Result<()>;
}

/// Process-global registry of live call UUIDs.
///
/// A UUID is inserted by [`crate::actor::init_call`] when a session is first
/// seen by the I/O callbacks and removed in `kill_channel`. The event
/// subscription layer ([`crate::event_sub`]) checks this to decide whether an
/// inbound `voice_seat::command` event has a live target.
static LIVE_CALLS: std::sync::LazyLock<DashMap<String, ()>> =
    std::sync::LazyLock::new(DashMap::new);

/// Mark a call UUID as live. Idempotent.
pub fn register_call(uuid: &str) {
    LIVE_CALLS.insert(uuid.to_string(), ());
}

/// Remove a call UUID from the live set. Idempotent.
pub fn unregister_call(uuid: &str) {
    LIVE_CALLS.remove(uuid);
}

/// Returns `true` when `uuid` is a currently live call.
pub fn is_live_call(uuid: &str) -> bool {
    LIVE_CALLS.contains_key(uuid)
}

/// Remove all live calls (module shutdown).
pub fn clear_calls() {
    LIVE_CALLS.clear();
}

/// Shared `CallControl` singleton.
///
/// Lazily constructed from [`FfiControl`](crate::control::FfiControl) on first
/// access; cloned cheaply (`Arc`) into the orchestrator at call setup.
pub fn control() -> Arc<dyn CallControl> {
    use std::sync::OnceLock;
    static CONTROL: OnceLock<Arc<dyn CallControl>> = OnceLock::new();
    CONTROL
        .get_or_init(|| Arc::new(crate::control::FfiControl) as Arc<dyn CallControl>)
        .clone()
}
