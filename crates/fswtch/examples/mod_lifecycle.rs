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
        fswtch::log_info("mod_lifecycle", "fswtch_lifecycle_stats invoked");
        let Some(stream) = stream else {
            return fswtch::FALSE;
        };
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
    fn switch_module_load(module) for "mod_lifecycle" {
        fswtch::log_info("mod_lifecycle", "loading module");
        LOADS.fetch_add(1, Ordering::Relaxed);
        module.api(
            "fswtch_lifecycle_stats",
            "prints module lifecycle counters",
            "fswtch_lifecycle_stats",
            stats_api,
        )
    }
}

// FreeSWITCH invokes runtime callbacks using the module function-table ABI. Unlike `shutdown`,
// the `runtime` field has no newtype trampoline in `module_exports!`, so it returns the raw
// `switch_status_t` directly.
unsafe extern "C" fn switch_module_runtime() -> fswtch::sys::switch_status_t {
    RUNTIME_TICKS.fetch_add(1, Ordering::Relaxed);
    fswtch::log_info("mod_lifecycle", "runtime tick");
    SUCCESS.raw()
}

// FreeSWITCH invokes shutdown callbacks using the module function-table ABI; the `module_exports!`
// macro bridges this safe `extern "C" fn` (returning `fswtch::Status`) to the raw status return.
extern "C" fn switch_module_shutdown() -> Status {
    SHUTDOWNS.fetch_add(1, Ordering::Relaxed);
    fswtch::log_info("mod_lifecycle", "shutdown callback invoked");
    SUCCESS
}
