use fswtch::{ModuleBuilder, SUCCESS, Status, sys};

fswtch::module_exports! {
    module = mod_app_playback_control,
    load = switch_module_load,
}

fswtch::app_callback! {
fn playback_control_app(session, data) {
    fswtch::log_info("mod_app_playback_control", "dialplan application invoked");
    let Some(session) = session else {
        fswtch::log_info("mod_app_playback_control", "missing session");
        return;
    };

    let Some(file) = data else {
        fswtch::log_info(
            "mod_app_playback_control",
            "no playback target supplied; sleeping",
        );
        let _ = session.sleep_ms(250);
        return;
    };

    let Ok(file) = fswtch::cstring(file) else {
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
}

fswtch::api_callback! {
fn info_api(_cmd, _session, stream) {
    fswtch::log_info(
        "mod_app_playback_control",
        "rust_playback_control_info invoked",
    );
    stream.write("application rust_playback_control registered; use from dialplan with a file path\n")
}
}

// SAFETY: FreeSWITCH calls this function during module load with loader-owned pointers.
unsafe extern "C" fn switch_module_load(
    module_interface: *mut *mut sys::switch_loadable_module_interface_t,
    pool: *mut sys::switch_memory_pool_t,
) -> Status {
    fswtch::log_info("mod_app_playback_control", "loading module");
    match ModuleBuilder::new(module_interface, pool, c"mod_app_playback_control")
        .and_then(|module| {
            module.application(
                c"rust_playback_control",
                c"Answers a channel and plays the supplied file path",
                c"Rust playback control example",
                c"rust_playback_control <path-or-tone-stream>",
                playback_control_app,
            )
        })
        .and_then(|module| {
            module.api(
                c"rust_playback_control_info",
                c"describes the Rust playback control application",
                c"rust_playback_control_info",
                info_api,
            )
        }) {
        Ok(_) => SUCCESS,
        Err(error) => error.0,
    }
}
