use std::ffi::{CStr, c_char};

use fswtch::{FALSE, Module, SUCCESS, Status, Stream, sys};

fswtch::module_exports! {
    module = mod_app_playback_control,
    load = switch_module_load,
}

// SAFETY: FreeSWITCH calls this function with pointers matching `switch_application_function_t`.
unsafe extern "C" fn playback_control_app(
    session: *mut sys::switch_core_session_t,
    data: *const c_char,
) {
    if session.is_null() {
        return;
    }

    let Some(file) = command_text(data) else {
        // SAFETY: `session` is provided by FreeSWITCH for this application invocation.
        let _ = unsafe {
            sys::switch_ivr_sleep(
                session,
                250,
                sys::switch_bool_t_SWITCH_FALSE,
                std::ptr::null_mut(),
            )
        };
        return;
    };

    let Ok(file) = std::ffi::CString::new(file) else {
        return;
    };

    // SAFETY: `session` is live for the duration of the application callback.
    let channel = unsafe { sys::switch_core_session_get_channel(session) };
    if !channel.is_null() {
        // SAFETY: `channel` belongs to `session`; the source strings are static C strings.
        let _ = unsafe {
            sys::switch_channel_perform_answer(
                channel,
                c"mod_app_playback_control.rs".as_ptr(),
                c"playback_control_app".as_ptr(),
                line!() as _,
            )
        };
    }

    // SAFETY: `session` is live and `file` is a valid C string for the duration of the call.
    let _ = unsafe {
        sys::switch_ivr_play_file(
            session,
            std::ptr::null_mut(),
            file.as_ptr(),
            std::ptr::null_mut(),
        )
    };
}

// SAFETY: FreeSWITCH calls this function with pointers matching `switch_api_function_t`.
unsafe extern "C" fn info_api(
    _cmd: *const c_char,
    _session: *mut sys::switch_core_session_t,
    stream: *mut sys::switch_stream_handle_t,
) -> Status {
    write_response(
        stream,
        "application rust_playback_control registered; use from dialplan with a file path\n",
    )
}

// SAFETY: FreeSWITCH calls this function during module load with loader-owned pointers.
unsafe extern "C" fn switch_module_load(
    module_interface: *mut *mut sys::switch_loadable_module_interface_t,
    pool: *mut sys::switch_memory_pool_t,
) -> Status {
    // SAFETY: The loader passes the module slot and pool, and the module name is static.
    let module =
        match unsafe { Module::create(module_interface, pool, c"mod_app_playback_control") } {
            Ok(module) => module,
            Err(error) => return error.0,
        };

    // SAFETY: The module interface is live, and assigned C strings/function pointer are static.
    if unsafe { add_application(module.as_ptr()) }.is_none() {
        return fswtch::GENERR;
    }

    // SAFETY: The callback and C strings remain valid for the loaded module lifetime.
    if let Err(error) = unsafe {
        module.add_api(
            c"rust_playback_control_info",
            c"describes the Rust playback control application",
            c"rust_playback_control_info",
            info_api,
        )
    } {
        return error.0;
    }

    SUCCESS
}

unsafe fn add_application(
    module: *mut sys::switch_loadable_module_interface_t,
) -> Option<*mut sys::switch_application_interface_t> {
    // SAFETY: `module` is a live module interface created by FreeSWITCH.
    let raw = unsafe {
        sys::switch_loadable_module_create_interface(
            module,
            sys::switch_module_interface_name_t::SWITCH_APPLICATION_INTERFACE,
        )
    }
    .cast::<sys::switch_application_interface_t>();
    if raw.is_null() {
        return None;
    }

    // SAFETY: `raw` points to a FreeSWITCH application interface allocation.
    unsafe {
        (*raw).interface_name = c"rust_playback_control".as_ptr();
        (*raw).application_function = Some(playback_control_app);
        (*raw).long_desc = c"Answers a channel and plays the supplied file path".as_ptr();
        (*raw).short_desc = c"Rust playback control example".as_ptr();
        (*raw).syntax = c"rust_playback_control <path-or-tone-stream>".as_ptr();
    }

    Some(raw)
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

fn write_response(stream: *mut sys::switch_stream_handle_t, text: &str) -> Status {
    // SAFETY: FreeSWITCH provides a valid stream pointer for the duration of the API callback.
    let Some(mut stream) = (unsafe { Stream::from_raw(stream) }) else {
        return FALSE;
    };

    match stream.write_str(text) {
        Ok(()) => SUCCESS,
        Err(error) => error.0,
    }
}
