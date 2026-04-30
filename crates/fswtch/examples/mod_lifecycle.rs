use std::sync::atomic::{AtomicUsize, Ordering};

use fswtch::{ModuleBuilder, SUCCESS, Status, sys};

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

// SAFETY: FreeSWITCH calls this function during module load with loader-owned pointers.
unsafe extern "C" fn switch_module_load(
    module_interface: *mut *mut sys::switch_loadable_module_interface_t,
    pool: *mut sys::switch_memory_pool_t,
) -> Status {
    fswtch::log_info("mod_lifecycle", "loading module");
    LOADS.fetch_add(1, Ordering::Relaxed);
    match ModuleBuilder::new(module_interface, pool, c"mod_lifecycle").and_then(|module| {
        module.api(
            c"rust_lifecycle_stats",
            c"prints module lifecycle counters",
            c"rust_lifecycle_stats",
            stats_api,
        )
    }) {
        Ok(_) => SUCCESS,
        Err(error) => error.0,
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
