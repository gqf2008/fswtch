use std::ffi::c_char;

use fswtch::{Module, SUCCESS, Session, Status, sys};

fswtch::module_exports! {
    module = mod_app_playback_control,
    load = switch_module_load,
}

// SAFETY: FreeSWITCH calls this function with pointers matching `switch_application_function_t`.
unsafe extern "C" fn playback_control_app(
    session: *mut sys::switch_core_session_t,
    data: *const c_char,
) {
    fswtch::log_info("mod_app_playback_control", "dialplan application invoked");
    let Some(session) = Session::from_raw(session) else {
        fswtch::log_info("mod_app_playback_control", "missing session");
        return;
    };

    let Some(file) = fswtch::command_text(data) else {
        fswtch::log_info(
            "mod_app_playback_control",
            "no playback target supplied; sleeping",
        );
        let _ = session.sleep_ms(250);
        return;
    };

    let Ok(file) = std::ffi::CString::new(file) else {
        fswtch::log_info(
            "mod_app_playback_control",
            "playback target contained NUL byte",
        );
        return;
    };

    let _ = session.answer();
    let _ = session.play_file(&file);
    fswtch::log_info("mod_app_playback_control", "playback call returned");
}

// SAFETY: FreeSWITCH calls this function with pointers matching `switch_api_function_t`.
unsafe extern "C" fn info_api(
    _cmd: *const c_char,
    _session: *mut sys::switch_core_session_t,
    stream: *mut sys::switch_stream_handle_t,
) -> Status {
    fswtch::log_info(
        "mod_app_playback_control",
        "rust_playback_control_info invoked",
    );
    fswtch::write_stream_response(
        stream,
        "application rust_playback_control registered; use from dialplan with a file path\n",
    )
}

// SAFETY: FreeSWITCH calls this function during module load with loader-owned pointers.
unsafe extern "C" fn switch_module_load(
    module_interface: *mut *mut sys::switch_loadable_module_interface_t,
    pool: *mut sys::switch_memory_pool_t,
) -> Status {
    fswtch::log_info("mod_app_playback_control", "loading module");
    let module = match Module::create(module_interface, pool, c"mod_app_playback_control") {
        Ok(module) => module,
        Err(error) => return error.0,
    };

    if let Err(error) = module.add_application(
        c"rust_playback_control",
        c"Answers a channel and plays the supplied file path",
        c"Rust playback control example",
        c"rust_playback_control <path-or-tone-stream>",
        playback_control_app,
    ) {
        return error.0;
    }

    if let Err(error) = module.add_api(
        c"rust_playback_control_info",
        c"describes the Rust playback control application",
        c"rust_playback_control_info",
        info_api,
    ) {
        return error.0;
    }

    SUCCESS
}
