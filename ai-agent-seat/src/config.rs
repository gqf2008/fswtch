//! AI agent seat configuration.
//!
//! Loads a [`voice_core::Config`] (AI endpoints, VAD params, audio settings)
//! from a YAML file at module-load time and stores it in a process-global
//! [`OnceLock`]. The module's `switch_module_load` resolves the YAML path —
//! typically from `VOICE_SEAT_CONFIG` or a FreeSWITCH XML setting — and calls
//! [`load`] before starting the runtime and registering the dialplan app, so a
//! fast inbound call can't race a pending load.
//!
//! The loaded [`Config`] is cloneable; [`get`] snapshots it per call (cheap —
//! the orchestrator params are derived from it at call setup). If [`load`]
//! hasn't run yet (or failed), [`get`] returns `None` so callers fall back to
//! defaults rather than aborting the call.

use std::sync::{Mutex, OnceLock};

use anyhow::Result;

use crate::voice_core::Config;

/// Process-global storage for the loaded [`Config`].
///
/// Wrapped in a `Mutex<Option<Config>>` so [`load`] can replace a previously
/// loaded value and [`get`] can clone it out without holding the lock across
/// I/O. The `OnceLock` only initializes the inner `Mutex`; the `Option` starts
/// `None` and is filled by [`load`].
static CONFIG: OnceLock<Mutex<Option<Config>>> = OnceLock::new();

/// Returns the static config cell, initializing the `Mutex` on first access.
fn config_cell() -> &'static Mutex<Option<Config>> {
    CONFIG.get_or_init(|| Mutex::new(None))
}

/// Load the AI [`Config`] from a YAML file at `path` and store it globally.
///
/// Delegates the actual YAML parsing to [`Config::load`]
/// (`voice_core::Config`), which reads the file and deserializes it via
/// `serde_yaml`. On success the new config replaces any previously loaded one;
/// on error the previous config (if any) is left untouched and the error is
/// returned, so the caller can log it and decide whether to fall back to
/// [`Config::default`].
pub fn load(path: &str) -> Result<()> {
    let config = Config::load(path)?;
    let mut guard = config_cell().lock().expect("config mutex poisoned");
    *guard = Some(config);
    Ok(())
}

/// Snapshot the loaded [`Config`].
///
/// Returns `None` if [`load`] hasn't run yet (e.g. a call races module load) or
/// if the last [`load`] failed. Callers should fall back to sensible defaults
/// in that case rather than failing the call.
pub fn get() -> Option<Config> {
    config_cell().lock().expect("config mutex poisoned").clone()
}
