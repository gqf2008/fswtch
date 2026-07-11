fswtch::module_exports! {
    module = mod_api_suite,
    load = switch_module_load,
}

fswtch::api_callback! {
    fn ping_api(_cmd, _session, stream) {
        fswtch::log_info("mod_api_suite", "fswtch_ping invoked");
        let Some(stream) = stream else {
            return fswtch::FALSE;
        };
        stream.write("pong\n")
    }
}

fswtch::api_callback! {
    fn echo_api(cmd, _session, stream) {
        fswtch::log_info("mod_api_suite", "fswtch_echo invoked");
        let Some(stream) = stream else {
            return fswtch::FALSE;
        };
        let text = cmd.unwrap_or_default();
        stream.write(&format!("{text}\n"))
    }
}

fswtch::api_callback! {
    fn upper_api(cmd, _session, stream) {
        fswtch::log_info("mod_api_suite", "fswtch_upper invoked");
        let Some(stream) = stream else {
            return fswtch::FALSE;
        };
        let text = cmd.unwrap_or_default();
        stream.write(&format!("{}\n", text.to_uppercase()))
    }
}

fswtch::module_load! {
    fn switch_module_load(module) for "mod_api_suite" {
        fswtch::log_info("mod_api_suite", "loading module");
        module
            .api("fswtch_ping", "returns pong", "fswtch_ping", ping_api)
            .and_then(|module| {
                module.api(
                    "fswtch_echo",
                    "echoes the command argument",
                    "fswtch_echo <text>",
                    echo_api,
                )
            })
            .and_then(|module| {
                module.api(
                    "fswtch_upper",
                    "uppercases the command argument",
                    "fswtch_upper <text>",
                    upper_api,
                )
            })
    }
}
