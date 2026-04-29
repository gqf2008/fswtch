use std::ffi::c_char;

use fswtch::{Module, SUCCESS, Status, Stream, sys};

fswtch::module_exports! {
    module = mod_hello,
    load = switch_module_load,
}

unsafe extern "C" fn hello_api(
    _cmd: *const c_char,
    _session: *mut sys::switch_core_session_t,
    stream: *mut sys::switch_stream_handle_t,
) -> Status {
    if let Some(mut stream) = unsafe { Stream::from_raw(stream) } {
        let _ = stream.write_str("hello from Rust\n");
    }

    SUCCESS
}

unsafe extern "C" fn switch_module_load(
    module_interface: *mut *mut sys::switch_loadable_module_interface_t,
    pool: *mut sys::switch_memory_pool_t,
) -> Status {
    let module = match unsafe { Module::create(module_interface, pool, c"mod_hello") } {
        Ok(module) => module,
        Err(error) => return error.0,
    };

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
