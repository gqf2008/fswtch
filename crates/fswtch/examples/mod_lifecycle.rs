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

unsafe extern "C" fn stats_api(
    _cmd: *const c_char,
    _session: *mut sys::switch_core_session_t,
    stream: *mut sys::switch_stream_handle_t,
) -> Status {
    if let Some(mut stream) = unsafe { Stream::from_raw(stream) } {
        let _ = stream.write_str(&format!(
            "loads={} runtime_ticks={} shutdowns={}\n",
            LOADS.load(Ordering::Relaxed),
            RUNTIME_TICKS.load(Ordering::Relaxed),
            SHUTDOWNS.load(Ordering::Relaxed)
        ));
    }

    SUCCESS
}

unsafe extern "C" fn switch_module_load(
    module_interface: *mut *mut sys::switch_loadable_module_interface_t,
    pool: *mut sys::switch_memory_pool_t,
) -> Status {
    LOADS.fetch_add(1, Ordering::Relaxed);

    let module = match unsafe { Module::create(module_interface, pool, c"mod_lifecycle") } {
        Ok(module) => module,
        Err(error) => return error.0,
    };

    if let Err(error) = unsafe {
        module.add_api(
            c"rust_lifecycle_stats",
            c"prints module lifecycle counters",
            c"rust_lifecycle_stats",
            stats_api,
        )
    } {
        return error.0;
    }

    SUCCESS
}

unsafe extern "C" fn switch_module_runtime() -> Status {
    RUNTIME_TICKS.fetch_add(1, Ordering::Relaxed);
    SUCCESS
}

unsafe extern "C" fn switch_module_shutdown() -> Status {
    SHUTDOWNS.fetch_add(1, Ordering::Relaxed);
    SUCCESS
}
