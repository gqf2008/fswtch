use std::ffi::c_char;

use fswtch::{Module, SUCCESS, Status, Stream, sys};

fswtch::module_exports! {
    module = mod_api_suite,
    load = switch_module_load,
}

// SAFETY: FreeSWITCH calls this function with pointers matching `switch_api_function_t`.
unsafe extern "C" fn ping_api(
    _cmd: *const c_char,
    _session: *mut sys::switch_core_session_t,
    stream: *mut sys::switch_stream_handle_t,
) -> Status {
    fswtch::log_info("mod_api_suite", "rust_ping invoked");
    write_response(stream, "pong\n")
}

// SAFETY: FreeSWITCH calls this function with pointers matching `switch_api_function_t`.
unsafe extern "C" fn echo_api(
    cmd: *const c_char,
    _session: *mut sys::switch_core_session_t,
    stream: *mut sys::switch_stream_handle_t,
) -> Status {
    fswtch::log_info("mod_api_suite", "rust_echo invoked");
    let text = fswtch::command_text(cmd).unwrap_or_default();
    write_response(stream, &format!("{text}\n"))
}

// SAFETY: FreeSWITCH calls this function with pointers matching `switch_api_function_t`.
unsafe extern "C" fn upper_api(
    cmd: *const c_char,
    _session: *mut sys::switch_core_session_t,
    stream: *mut sys::switch_stream_handle_t,
) -> Status {
    fswtch::log_info("mod_api_suite", "rust_upper invoked");
    let text = fswtch::command_text(cmd).unwrap_or_default();
    write_response(stream, &format!("{}\n", text.to_uppercase()))
}

// SAFETY: FreeSWITCH calls this function during module load with loader-owned pointers.
unsafe extern "C" fn switch_module_load(
    module_interface: *mut *mut sys::switch_loadable_module_interface_t,
    pool: *mut sys::switch_memory_pool_t,
) -> Status {
    fswtch::log_info("mod_api_suite", "loading module");
    let module = match Module::create(module_interface, pool, c"mod_api_suite") {
        Ok(module) => module,
        Err(error) => return error.0,
    };

    for result in [
        module.add_api(c"rust_ping", c"returns pong", c"rust_ping", ping_api),
        module.add_api(
            c"rust_echo",
            c"echoes the command argument",
            c"rust_echo <text>",
            echo_api,
        ),
        module.add_api(
            c"rust_upper",
            c"uppercases the command argument",
            c"rust_upper <text>",
            upper_api,
        ),
    ] {
        if let Err(error) = result {
            return error.0;
        }
    }

    SUCCESS
}

fn write_response(stream: *mut sys::switch_stream_handle_t, text: &str) -> Status {
    // SAFETY: FreeSWITCH provides a valid stream pointer for the duration of the API callback.
    if let Some(mut stream) = Stream::from_raw(stream)
        && let Err(error) = stream.write_str(text)
    {
        return error.0;
    }

    SUCCESS
}
