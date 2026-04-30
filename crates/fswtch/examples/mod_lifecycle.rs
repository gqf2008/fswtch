use std::{
    ffi::c_char,
    sync::atomic::{AtomicUsize, Ordering},
};

use fswtch::{Module, SUCCESS, Status, Stream, sys};

static LOADS: AtomicUsize = AtomicUsize::new(0);
static RUNTIME_TICKS: AtomicUsize = AtomicUsize::new(0);
static SHUTDOWNS: AtomicUsize = AtomicUsize::new(0);

fswtch::module_exports! {
    module = mod_lifecycle,
    load = switch_module_load,
    shutdown = Some(switch_module_shutdown),
    runtime = Some(switch_module_runtime),
}

// SAFETY: FreeSWITCH calls this function with pointers matching `switch_api_function_t`.
unsafe extern "C" fn stats_api(
    _cmd: *const c_char,
    _session: *mut sys::switch_core_session_t,
    stream: *mut sys::switch_stream_handle_t,
) -> Status {
    fswtch::log_info("mod_lifecycle", "rust_lifecycle_stats invoked");
    // SAFETY: FreeSWITCH provides a valid stream pointer for the duration of the API callback.
    let Some(mut stream) = Stream::from_raw(stream) else {
        return SUCCESS;
    };
    if let Err(error) = stream.write_str(&format!(
        "loads={} runtime_ticks={} shutdowns={}\n",
        LOADS.load(Ordering::Relaxed),
        RUNTIME_TICKS.load(Ordering::Relaxed),
        SHUTDOWNS.load(Ordering::Relaxed)
    )) {
        return error.0;
    }

    SUCCESS
}

// SAFETY: FreeSWITCH calls this function during module load with loader-owned pointers.
unsafe extern "C" fn switch_module_load(
    module_interface: *mut *mut sys::switch_loadable_module_interface_t,
    pool: *mut sys::switch_memory_pool_t,
) -> Status {
    fswtch::log_info("mod_lifecycle", "loading module");
    LOADS.fetch_add(1, Ordering::Relaxed);
    let module = match Module::create(module_interface, pool, c"mod_lifecycle") {
        Ok(module) => module,
        Err(error) => return error.0,
    };
    if let Err(error) = module.add_api(
        c"rust_lifecycle_stats",
        c"prints module lifecycle counters",
        c"rust_lifecycle_stats",
        stats_api,
    ) {
        return error.0;
    }

    SUCCESS
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
