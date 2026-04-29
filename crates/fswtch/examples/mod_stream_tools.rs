use std::ffi::{CStr, c_char};

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
    // SAFETY: FreeSWITCH provides a valid stream pointer for the duration of the API callback.
    let Some(mut stream) = (unsafe { Stream::from_raw(stream) }) else {
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
    // SAFETY: FreeSWITCH provides a valid stream pointer for the duration of the API callback.
    let Some(mut stream) = (unsafe { Stream::from_raw(stream) }) else {
        return FALSE;
    };

    let Some(text) = command_text(cmd) else {
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
    // SAFETY: The loader passes the module slot and pool, and the module name is static.
    let module = match unsafe { Module::create(module_interface, pool, c"mod_stream_tools") } {
        Ok(module) => module,
        Err(error) => return error.0,
    };

    for result in [
        // SAFETY: The callback and C strings remain valid for the loaded module lifetime.
        unsafe {
            module.add_api(
                c"rust_table",
                c"prints a small CSV response",
                c"rust_table",
                table_api,
            )
        },
        // SAFETY: The callback and C strings remain valid for the loaded module lifetime.
        unsafe {
            module.add_api(
                c"rust_words",
                c"counts words in the command argument",
                c"rust_words <text>",
                words_api,
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

    // SAFETY: FreeSWITCH passes a null-terminated command string when one is present.
    unsafe { CStr::from_ptr(cmd) }
        .to_str()
        .ok()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}
