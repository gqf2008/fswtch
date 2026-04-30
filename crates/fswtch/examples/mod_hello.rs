use std::ffi::c_char;

use fswtch::{Module, SUCCESS, Status, Stream, sys};

fswtch::module_exports! {
    module = mod_hello,
    load = switch_module_load,
}

// SAFETY: FreeSWITCH calls this function with pointers matching `switch_api_function_t`.
unsafe extern "C" fn hello_api(
    _cmd: *const c_char,
    _session: *mut sys::switch_core_session_t,
    stream: *mut sys::switch_stream_handle_t,
) -> Status {
    fswtch::log_info("mod_hello", "rust_hello invoked");
    // SAFETY: FreeSWITCH provides a valid stream pointer for the duration of the API callback.
    let Some(mut stream) = (unsafe { Stream::from_raw(stream) }) else {
        fswtch::log_info("mod_hello", "missing stream handle");
        return SUCCESS;
    };
    if let Err(error) = stream.write_str("hello from Rust\n") {
        return error.0;
    }

    SUCCESS
}

// SAFETY: FreeSWITCH calls this function during module load with loader-owned pointers.
unsafe extern "C" fn switch_module_load(
    module_interface: *mut *mut sys::switch_loadable_module_interface_t,
    pool: *mut sys::switch_memory_pool_t,
) -> Status {
    fswtch::log_info("mod_hello", "loading module");
    // SAFETY: The loader passes the module slot and pool, and the module name is static.
    let module = match unsafe { Module::create(module_interface, pool, c"mod_hello") } {
        Ok(module) => module,
        Err(error) => return error.0,
    };

    // SAFETY: The callback and C strings remain valid for the loaded module lifetime.
    if let Err(error) = unsafe {
        module.add_api(
            c"rust_hello",
            c"prints a Rust greeting",
            c"rust_hello",
            hello_api,
        )
    } {
        return error.0;
    }

    SUCCESS
}
