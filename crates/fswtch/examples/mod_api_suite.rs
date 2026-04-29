use std::ffi::{CStr, c_char};

use fswtch::{Module, SUCCESS, Status, Stream, sys};

fswtch::module_exports! {
    module = mod_api_suite,
    load = switch_module_load,
}

unsafe extern "C" fn ping_api(
    _cmd: *const c_char,
    _session: *mut sys::switch_core_session_t,
    stream: *mut sys::switch_stream_handle_t,
) -> Status {
    write_response(stream, "pong\n")
}

unsafe extern "C" fn echo_api(
    cmd: *const c_char,
    _session: *mut sys::switch_core_session_t,
    stream: *mut sys::switch_stream_handle_t,
) -> Status {
    let text = command_text(cmd).unwrap_or_default();
    write_response(stream, &format!("{text}\n"))
}

unsafe extern "C" fn upper_api(
    cmd: *const c_char,
    _session: *mut sys::switch_core_session_t,
    stream: *mut sys::switch_stream_handle_t,
) -> Status {
    let text = command_text(cmd).unwrap_or_default();
    write_response(stream, &format!("{}\n", text.to_uppercase()))
}

unsafe extern "C" fn switch_module_load(
    module_interface: *mut *mut sys::switch_loadable_module_interface_t,
    pool: *mut sys::switch_memory_pool_t,
) -> Status {
    let module = match unsafe { Module::create(module_interface, pool, c"mod_api_suite") } {
        Ok(module) => module,
        Err(error) => return error.0,
    };

    for result in [
        unsafe { module.add_api(c"rust_ping", c"returns pong", c"rust_ping", ping_api) },
        unsafe {
            module.add_api(
                c"rust_echo",
                c"echoes the command argument",
                c"rust_echo <text>",
                echo_api,
            )
        },
        unsafe {
            module.add_api(
                c"rust_upper",
                c"uppercases the command argument",
                c"rust_upper <text>",
                upper_api,
            )
        },
    ] {
        if let Err(error) = result {
            return error.0;
        }
    }

    SUCCESS
}

fn command_text(cmd: *const c_char) -> Option<String> {
    if cmd.is_null() {
        return None;
    }

    unsafe { CStr::from_ptr(cmd) }
        .to_str()
        .ok()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}

fn write_response(stream: *mut sys::switch_stream_handle_t, text: &str) -> Status {
    if let Some(mut stream) = unsafe { Stream::from_raw(stream) }
        && let Err(error) = stream.write_str(text)
    {
        return error.0;
    }

    SUCCESS
}
