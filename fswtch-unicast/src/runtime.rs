//! Tokio runtime lifecycle for fswtch_unicast.
//!
//! The endpoint-module design has no async actors inside the FreeSWITCH media
//! thread. The only async surface is per-call UDP I/O, which is driven by tokio
//! tasks spawned from `io::CallState::new`.
//!
//! Lifecycle:
//! - [`start()`] (on the FS module-load thread) builds a multi-threaded
//!   `Runtime` and stores it in a global `OnceLock`.
//! - [`spawn()`] (callable from any FS media thread) hands the future to the
//!   runtime via `Runtime::spawn` — the future lands on the runtime's worker
//!   pool, NOT the media thread, so it never blocks the 50 Hz frame loop.
//! - [`stop()`] shuts the runtime down (module shutdown).

use std::sync::{Mutex, OnceLock};

use tokio::runtime::Runtime;

use fswtch::{GENERR, SwitchError, log_error, log_info, log_warning};

const MODULE: &str = "fswtch_unicast";

/// Global runtime slot. `OnceLock` gives a `&'static` reference lazily
/// initialized on first access (module load); the `Mutex<Option<_>>` allows
/// `stop()` to `take()` the runtime for teardown.
static RUNTIME: OnceLock<Mutex<Option<Runtime>>> = OnceLock::new();

fn runtime_slot() -> &'static Mutex<Option<Runtime>> {
    RUNTIME.get_or_init(|| Mutex::new(None))
}

/// Build the tokio `Runtime`.
///
/// Called from `switch_module_load` on the FS module-load thread. Safe to call
/// again — a double load logs a warning and returns `Ok(())` without
/// re-initializing.
pub fn start() -> fswtch::Result<()> {
    let mut guard = runtime_slot().lock().expect("runtime mutex poisoned");
    if guard.is_some() {
        log_warning(MODULE, "runtime already started — ignoring double load");
        return Ok(());
    }

    let runtime = Runtime::new().map_err(|e| {
        log_error(
            MODULE,
            format!("tokio Runtime creation failed: {e} — module load aborted"),
        );
        SwitchError::new(GENERR)
    })?;

    log_info(MODULE, "tokio Runtime started (multi-thread worker pool)");
    *guard = Some(runtime);
    Ok(())
}

/// Stop the tokio `Runtime`.
///
/// Called from `switch_module_shutdown` (after FS has torn down channels). No-op
/// (with a log) if the runtime was never started or already stopped.
pub fn stop() {
    let taken = runtime_slot()
        .lock()
        .expect("runtime mutex poisoned")
        .take();
    let Some(runtime) = taken else {
        log_warning(MODULE, "stop() called but runtime was not started");
        return;
    };
    runtime.shutdown_timeout(std::time::Duration::from_secs(5));
    log_info(MODULE, "tokio Runtime stopped");
}

/// Spawn a future on the tokio runtime from an arbitrary (non-tokio) thread.
///
/// Use case: the `write_frame` I/O callback (on FreeSWITCH's media thread)
/// launches a UDP send task. No-op (with a log) if the runtime isn't up yet
/// (e.g. during load races). The future runs on the runtime's worker pool, so it
/// never blocks the media thread.
pub fn spawn<F>(future: F) -> Option<tokio::task::JoinHandle<F::Output>>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let guard = runtime_slot().lock().expect("runtime mutex poisoned");
    let Some(runtime) = guard.as_ref() else {
        log_error(MODULE, "spawn() called before runtime start");
        return None;
    };
    Some(runtime.spawn(future))
}

/// Returns a handle to the live runtime, if started.
pub fn handle() -> Option<tokio::runtime::Handle> {
    let guard = runtime_slot().lock().expect("runtime mutex poisoned");
    guard.as_ref().map(|r| r.handle().clone())
}
