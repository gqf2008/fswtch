//! actix runtime lifecycle for ai_agent_seat.
//!
//! The seat runs its AI pipeline as actix actors (`CallActor`) plus detached
//! tokio tasks (ASR/LLM/TTS HTTP, speech drain). actix 0.13 actors require an
//! `actix::System` (tokio + `LocalSet` + `Arbiter`); a bare tokio multi-thread
//! runtime lacks the `LocalSet` so `actix::spawn` panics. So **the module's
//! runtime IS an `actix::System`**, not a hand-built tokio runtime.
//!
//! Lifecycle:
//! - `start()` (on the FS module-load thread) builds the `SystemRunner` on a
//!   dedicated driver thread, captures the system `ArbiterHandle` via a oneshot
//!   channel, then stores it in a global `OnceLock`.
//! - `spawn()` (callable from any FS media/app thread) targets that
//!   `ArbiterHandle`; the future lands on the System's runtime.
//! - `stop()` signals `System::stop()` from inside the arbiter (where
//!   `System::current()` is valid) and joins the driver thread.
//!
//! Storage uses `std::sync::OnceLock` (initialized exactly once at module
//! load; module load is single-threaded in FreeSWITCH). The inner `Mutex`
//! guards the optional teardown state — `start` writes, `stop` takes.

use std::sync::{Mutex, OnceLock};

use actix::{ArbiterHandle, System};

use fswtch::{SwitchError, GENERR, log_error, log_info, log_warning};

const MODULE: &str = "ai_agent_seat";

/// Bundle of the captured System handle + the driver thread join handle.
struct SystemRuntime {
    /// Handle to the System's default Arbiter. Clonable + `Send`; `spawn` on
    /// it from any thread lands the future on the System's runtime.
    arbiter: ArbiterHandle,
    /// The dedicated driver thread running `SystemRunner::run()`. Joined in
    /// `stop()` after `System::stop()` unblocks `run()`.
    driver: Option<std::thread::JoinHandle<()>>,
}

/// Global runtime slot. `OnceLock` gives a `&'static` reference lazily
/// initialized on first access (module load); the `Mutex<Option<_>>` allows
/// `stop()` to `take()` the runtime for teardown.
static SYSTEM: OnceLock<Mutex<Option<SystemRuntime>>> = OnceLock::new();

fn system_slot() -> &'static Mutex<Option<SystemRuntime>> {
    SYSTEM.get_or_init(|| Mutex::new(None))
}

/// Build the actix `System` and start its driver thread.
///
/// Called from `switch_module_load` on the FS module-load thread. Safe to
/// call again — a double load logs a warning and returns `Ok(())` without
/// re-initializing.
pub fn start() -> fswtch::Result<()> {
    let mut guard = system_slot().lock().expect("system mutex poisoned");
    if guard.is_some() {
        log_warning(MODULE, "system already started — ignoring double load");
        return Ok(());
    }

    // The actix `SystemRunner` is bound to a tokio `LocalSet` (thread-local),
    // so it is NOT `Send` — it cannot move across threads. It must be created
    // AND driven on the same dedicated thread. Create the System on the driver
    // thread; pass the `ArbiterHandle` back to the load thread via a oneshot.
    let (tx, rx) = std::sync::mpsc::channel::<ArbiterHandle>();
    let driver = std::thread::Builder::new()
        .name("ai-agent-seat-system".into())
        .spawn(move || {
            // Created + driven on THIS thread: `System::new()` builds the
            // tokio runtime + LocalSet + system Arbiter; `run()` blocks until
            // `System::stop()`.
            let runner = System::new();
            // `Arbiter::current()` is valid here — this thread is in the System
            // context once the runner is built.
            let arbiter = actix::Arbiter::current();
            let _ = tx.send(arbiter);
            let _ = runner.run();
        })
        .map_err(|e| {
            log_error(
                MODULE,
                format!("actix system driver thread spawn failed: {e} — module load aborted"),
            );
            SwitchError(GENERR)
        })?;

    let arbiter = rx.recv().map_err(|e| {
        log_error(
            MODULE,
            format!("actix System init failed on driver thread: {e} — module load aborted"),
        );
        SwitchError(GENERR)
    })?;

    log_info(MODULE, "actix System started (driver thread + system Arbiter)");
    *guard = Some(SystemRuntime {
        arbiter,
        driver: Some(driver),
    });
    Ok(())
}

/// Stop the actix `System` and join the driver thread.
///
/// Called from `switch_module_shutdown` (after FS has torn down channels). No-op
/// (with a log) if the system was never started or already stopped.
pub fn stop() {
    let mut taken = system_slot().lock().expect("system mutex poisoned").take();
    let Some(mut sys) = taken.take() else {
        log_warning(MODULE, "stop() called but system was not started");
        return;
    };

    // Signal System::stop() from *inside* the arbiter (where System::current()
    // is valid). This unblocks `SystemRunner::run()` on the driver thread.
    sys.arbiter.spawn(async {
        System::current().stop();
    });

    if let Some(handle) = sys.driver.take() {
        if let Err(e) = handle.join() {
            log_warning(MODULE, format!("system driver thread join failed: {e:?}"));
        }
    }
    log_info(MODULE, "actix System stopped");
}

/// Spawn a future on the System's Arbiter from an arbitrary (non-actix) thread.
///
/// Use case: an FS media/app thread creating a per-call task (e.g. driving an
/// AI pipeline turn). No-op (with a log) if the system isn't up yet (e.g.
/// during load races). `ArbiterHandle::spawn` returns only a `bool` acceptance
/// flag, which we discard — all current callers run detached.
pub fn spawn<F>(future: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let guard = system_slot().lock().expect("system mutex poisoned");
    let Some(sys) = guard.as_ref() else {
        log_error(MODULE, "spawn() called before system start");
        return;
    };
    sys.arbiter.spawn(future);
}
