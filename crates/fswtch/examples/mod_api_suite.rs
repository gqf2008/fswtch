fswtch::module_exports! {
    module = mod_api_suite,
    load = switch_module_load,
}

fswtch::api_callback! {
fn ping_api(_cmd, _session, stream) {
    fswtch::log_info("mod_api_suite", "rust_ping invoked");
    stream.write("pong\n")
}
}

fswtch::api_callback! {
fn echo_api(cmd, _session, stream) {
    fswtch::log_info("mod_api_suite", "rust_echo invoked");
    let text = cmd.unwrap_or_default();
    stream.write(&format!("{text}\n"))
}
}

fswtch::api_callback! {
fn upper_api(cmd, _session, stream) {
    fswtch::log_info("mod_api_suite", "rust_upper invoked");
    let text = cmd.unwrap_or_default();
    stream.write(&format!("{}\n", text.to_uppercase()))
}
}

fswtch::module_load! {
    fn switch_module_load(module) for "mod_api_suite" {
        fswtch::log_info("mod_api_suite", "loading module");
        module.api("rust_ping", "returns pong", "rust_ping", ping_api)
        .and_then(|module| {
            module.api(
                "rust_echo",
                "echoes the command argument",
                "rust_echo <text>",
                echo_api,
            )
        })
        .and_then(|module| {
            module.api(
                "rust_upper",
                "uppercases the command argument",
                "rust_upper <text>",
                upper_api,
            )
        })
    }
}
