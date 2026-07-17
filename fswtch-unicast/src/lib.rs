//! FreeSWITCH endpoint module `fswtch_unicast`.
//!
//! Registers an endpoint interface named `fswtch_unicast`. Bridging a call to
//! `fswtch_unicast/<ip>:<port>` creates a B-leg whose media is forwarded over a
//! single raw-PCM UDP socket to the peer at `<ip>:<port>`: caller audio is sent
//! out as little-endian i16 PCM, and PCM received on the same socket is played
//! back toward the caller. There is no framing and no signalling — one UDP
//! socket per call, raw PCM in both directions. This is the single-channel
//! UDP-media analogue of FreeSWITCH's built-in `mod_unicast`.

pub mod io;
pub mod runtime;

fswtch::module_exports! {
    module = fswtch_unicast,
    load = switch_module_load,
    shutdown = Some(switch_module_shutdown),
    runtime = None,
}

fn do_module_load(module: fswtch::ModuleBuilder) -> fswtch::Result<fswtch::ModuleBuilder> {
    init_tracing();
    tracing::info!("Loading fswtch_unicast module");

    // Start the process-global tokio runtime used for UDP I/O tasks.
    crate::runtime::start()?;

    // Build the I/O routines table and register the endpoint interface.
    let io = fswtch::EndpointIoBuilder::build::<io::FswtchUnicast>()?;
    let state_handler = fswtch::StateHandlerTable::new_null();
    let module = module.endpoint("fswtch_unicast", io, state_handler)?;

    tracing::info!("fswtch_unicast module loaded successfully");
    Ok(module)
}

fswtch::module_load! {
    fn switch_module_load(module) for "fswtch_unicast" {
        do_module_load(module)
    }
}

/// Module shutdown function.
pub extern "C" fn switch_module_shutdown() -> fswtch::Status {
    tracing::info!("Shutting down fswtch_unicast module");

    // Drop per-call state while the tokio runtime is still alive so UDP tasks
    // can be aborted cleanly.
    io::CALLS.clear();

    // Stop the runtime last to drain any abort-handling tasks.
    crate::runtime::stop();

    tracing::info!("fswtch_unicast module shutdown complete");
    fswtch::SUCCESS
}

// ── tracing → FreeSWITCH switch_log_printf bridge ──────────────────────────

use std::fmt::Write as _;
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::Context as LayerContext;
use tracing_subscriber::{Layer, layer::SubscriberExt, util::SubscriberInitExt};

struct FsLogLayer;

impl<S: tracing::Subscriber> Layer<S> for FsLogLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: LayerContext<'_, S>) {
        let mut visitor = FieldCollector::default();
        event.record(&mut visitor);

        let meta = event.metadata();
        let level = match *meta.level() {
            tracing::Level::ERROR => fswtch::LogLevel::Error,
            tracing::Level::WARN => fswtch::LogLevel::Warning,
            tracing::Level::INFO => fswtch::LogLevel::Info,
            tracing::Level::DEBUG => fswtch::LogLevel::Debug,
            tracing::Level::TRACE => fswtch::LogLevel::Debug2,
        };
        fswtch::log(meta.target(), level, visitor.0);
    }
}

#[derive(Default)]
struct FieldCollector(String);

impl Visit for FieldCollector {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if !self.0.is_empty() {
            self.0.push(' ');
        }
        if field.name() == "message" {
            let _ = write!(self.0, "{:?}", value);
        } else {
            let _ = write!(self.0, "{}={:?}", field.name(), value);
        }
    }
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("fswtch_unicast=info"));
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(FsLogLayer)
        .try_init();
}
