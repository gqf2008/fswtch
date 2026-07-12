//! Panic boundary for FreeSWITCH thread callbacks.
//!
//! Every callback that FreeSWITCH invokes (the `voice_seat` app entry, the
//! media-bug handlers) crosses from an FS-native thread into Rust. Because the
//! module is built `panic = "unwind"` (not abort), a panic in that crossing
//! unwinds instead of killing the FS process. `catch_fs` traps it at the edge
//! and downgrades it to the default value of `R`, so a single panic becomes
//! "this frame / this call" loss rather than a process-wide abort.

use std::panic::{self, AssertUnwindSafe};

/// Run `f` under a panic guard.
///
/// Wraps `f` in `std::panic::catch_unwind(AssertUnwindSafe(f))`. On a normal
/// return the value is yielded back to the caller; on a panic the payload is
/// extracted into a message, logged via `fswtch::log_error`, and `R::default()`
/// is returned instead. Use at every FS→Rust entry point so a panic never
/// unwinds across the FFI boundary.
pub fn catch_fs<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
    R: Default,
{
    match panic::catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(payload) => {
            let msg = payload
                .downcast_ref::<&'static str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "<non-string panic>".to_string());
            fswtch::log_error(
                "ai_agent_seat",
                format!("panic caught at FS boundary (downgrading): {msg}"),
            );
            R::default()
        }
    }
}

/// Run `f` under a panic guard, returning [`fswtch::Status`].
///
/// Like [`catch_fs`] but specialized for I/O callbacks that return a
/// `switch_status_t` (which does not implement `Default`). On a panic the
/// status is [`fswtch::FALSE`] ("false" / non-success) so FreeSWITCH treats the
/// frame as not-produced and continues, rather than unwinding into the media
/// loop and crashing the process.
pub fn catch_fs_status<F>(f: F) -> fswtch::Status
where
    F: FnOnce() -> fswtch::Status,
{
    match panic::catch_unwind(AssertUnwindSafe(f)) {
        Ok(s) => s,
        Err(payload) => {
            let msg = payload
                .downcast_ref::<&'static str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "<non-string panic>".to_string());
            fswtch::log_error(
                "ai_agent_seat",
                format!("panic caught at FS boundary (downgrading to FALSE): {msg}"),
            );
            fswtch::FALSE
        }
    }
}
