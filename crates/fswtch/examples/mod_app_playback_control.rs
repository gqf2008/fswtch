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

    let _ = session.answer();
    let _ = session.play_file(file);
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

fswtch::module_load! {
    fn switch_module_load(module) for "mod_app_playback_control" {
        fswtch::log_info("mod_app_playback_control", "loading module");
        module
            .application(
                "rust_playback_control",
                "Answers a channel and plays the supplied file path",
                "Rust playback control example",
                "rust_playback_control <path-or-tone-stream>",
                playback_control_app,
            )
            .and_then(|module| {
                module.api(
                    "rust_playback_control_info",
                    "describes the Rust playback control application",
                    "rust_playback_control_info",
                    info_api,
                )
            })
    }
}
