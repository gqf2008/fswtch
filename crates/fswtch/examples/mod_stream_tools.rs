use std::ffi::c_char;

use fswtch::{FALSE, Module, SUCCESS, Status, Stream, sys};

fswtch::module_exports! {
    module = mod_stream_tools,
    load = switch_module_load,
}

// SAFETY: FreeSWITCH calls this function with pointers matching `switch_api_function_t`.
unsafe extern "C" fn table_api(
    _cmd: *const c_char,
    _session: *mut sys::switch_core_session_t,
    stream: *mut sys::switch_stream_handle_t,
) -> Status {
    fswtch::log_info("mod_stream_tools", "rust_table invoked");
    // SAFETY: FreeSWITCH provides a valid stream pointer for the duration of the API callback.
    let Some(mut stream) = Stream::from_raw(stream) else {
        return FALSE;
    };

    for line in [
        "name,value\n",
        "language,rust\n",
        "module,mod_stream_tools\n",
        "binding,fswtch\n",
    ] {
        if let Err(error) = stream.write_str(line) {
            return error.0;
        }
    }

    SUCCESS
}

// SAFETY: FreeSWITCH calls this function with pointers matching `switch_api_function_t`.
unsafe extern "C" fn words_api(
    cmd: *const c_char,
    _session: *mut sys::switch_core_session_t,
    stream: *mut sys::switch_stream_handle_t,
) -> Status {
    fswtch::log_info("mod_stream_tools", "rust_words invoked");
    // SAFETY: FreeSWITCH provides a valid stream pointer for the duration of the API callback.
    let Some(mut stream) = Stream::from_raw(stream) else {
        return FALSE;
    };

    let Some(text) = fswtch::command_text(cmd) else {
        return match stream.write_str("0 words\n") {
            Ok(()) => SUCCESS,
            Err(error) => error.0,
        };
    };

    let count = text.split_whitespace().count();
    if let Err(error) = stream.write_str(&format!("{count} words\n")) {
        return error.0;
    }

    SUCCESS
}

// SAFETY: FreeSWITCH calls this function during module load with loader-owned pointers.
unsafe extern "C" fn switch_module_load(
    module_interface: *mut *mut sys::switch_loadable_module_interface_t,
    pool: *mut sys::switch_memory_pool_t,
) -> Status {
    fswtch::log_info("mod_stream_tools", "loading module");
    let module = match Module::create(module_interface, pool, c"mod_stream_tools") {
        Ok(module) => module,
        Err(error) => return error.0,
    };

    for result in [
        module.add_api(
            c"rust_table",
            c"prints a small CSV response",
            c"rust_table",
            table_api,
        ),
        module.add_api(
            c"rust_words",
            c"counts words in the command argument",
            c"rust_words <text>",
            words_api,
        ),
    ] {
        if let Err(error) = result {
            return error.0;
        }
    }

    SUCCESS
}
