use std::sync::atomic::{AtomicUsize, Ordering};

use fswtch::{SUCCESS, Status};

static LOADS: AtomicUsize = AtomicUsize::new(0);
static RUNTIME_TICKS: AtomicUsize = AtomicUsize::new(0);
static SHUTDOWNS: AtomicUsize = AtomicUsize::new(0);

fswtch::module_exports! {
    module = mod_lifecycle,
    load = switch_module_load,
    shutdown = Some(switch_module_shutdown),
    runtime = Some(switch_module_runtime),
}

fswtch::api_callback! {
    fn stats_api(_cmd, _session, stream) {
        fswtch::log_info("mod_lifecycle", "rust_lifecycle_stats invoked");
        stream.write(
            &format!(
                "loads={} runtime_ticks={} shutdowns={}\n",
                LOADS.load(Ordering::Relaxed),
                RUNTIME_TICKS.load(Ordering::Relaxed),
                SHUTDOWNS.load(Ordering::Relaxed)
            ),
        )
    }
}

fswtch::module_load! {
    fn switch_module_load(module) for c"mod_lifecycle" {
        fswtch::log_info("mod_lifecycle", "loading module");
        LOADS.fetch_add(1, Ordering::Relaxed);
        module.api(
            c"rust_lifecycle_stats",
            c"prints module lifecycle counters",
            c"rust_lifecycle_stats",
            stats_api,
        )
    }
}

// SAFETY: FreeSWITCH invokes runtime callbacks using the module function-table ABI.
unsafe extern "C" fn switch_module_runtime() -> Status {
    RUNTIME_TICKS.fetch_add(1, Ordering::Relaxed);
    fswtch::log_info("mod_lifecycle", "runtime tick");
    SUCCESS
}

// SAFETY: FreeSWITCH invokes shutdown callbacks using the module function-table ABI.
unsafe extern "C" fn switch_module_shutdown() -> Status {
    SHUTDOWNS.fetch_add(1, Ordering::Relaxed);
    fswtch::log_info("mod_lifecycle", "shutdown callback invoked");
    SUCCESS
}
