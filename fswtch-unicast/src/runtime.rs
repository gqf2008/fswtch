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

/// Report a runtime-API misuse (called before [`start()`]). Test builds skip
/// the `log_error` call: it resolves FreeSWITCH's `switch_log_printf`, which
/// does not exist outside a FreeSWITCH process, and a test-reachable
/// reference to it fails to link the unit-test binary. Production behaviour
/// is unchanged.
#[inline]
fn log_misuse(msg: &str) {
    #[cfg(not(test))]
    log_error(MODULE, msg);
    #[cfg(test)]
    let _ = msg;
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
        log_misuse("spawn() called before runtime start");
        return None;
    };
    Some(runtime.spawn(future))
}

/// Drive a future to completion on the tokio runtime, blocking the calling
/// thread until it finishes.
///
/// Use case: `io::CallState::new` runs on FreeSWITCH's synchronous originate
/// thread, which has no tokio context — but `tokio::net::UdpSocket`
/// constructors require a reactor and panic without one. `Runtime::block_on`
/// is legal from any thread that is not itself inside a runtime; all FS
/// callback threads qualify. This is a once-per-call setup path, not the
/// 50 Hz media loop, so the brief block is acceptable. The slot lock is held
/// across the call so `stop()` cannot tear the runtime down mid-flight.
/// Returns `None` (with a log) if the runtime isn't started.
pub fn block_on<F: std::future::Future>(future: F) -> Option<F::Output> {
    let guard = runtime_slot().lock().expect("runtime mutex poisoned");
    let Some(runtime) = guard.as_ref() else {
        log_misuse("block_on() called before runtime start");
        return None;
    };
    Some(runtime.block_on(future))
}

/// Install a runtime for unit tests without any FreeSWITCH logging: the
/// `log_*` helpers resolve `switch_log_printf` at call time, which does not
/// exist in a test process. Idempotent, like [`start()`].
#[cfg(test)]
pub fn start_for_test() {
    let mut guard = runtime_slot().lock().expect("runtime mutex poisoned");
    if guard.is_none() {
        *guard = Some(Runtime::new().expect("test tokio runtime"));
    }
}
