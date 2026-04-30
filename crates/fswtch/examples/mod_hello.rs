use std::ffi::c_char;

use fswtch::{Module, SUCCESS, Status, sys};

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
    fswtch::write_stream_response(stream, "hello from Rust\n")
}

// SAFETY: FreeSWITCH calls this function during module load with loader-owned pointers.
unsafe extern "C" fn switch_module_load(
    module_interface: *mut *mut sys::switch_loadable_module_interface_t,
    pool: *mut sys::switch_memory_pool_t,
) -> Status {
    fswtch::log_info("mod_hello", "loading module");
    let module = match Module::create(module_interface, pool, c"mod_hello") {
        Ok(module) => module,
        Err(error) => return error.0,
    };
    if let Err(error) = module.add_api(
        c"rust_hello",
        c"prints a Rust greeting",
        c"rust_hello",
        hello_api,
    ) {
        return error.0;
    }

    SUCCESS
}
