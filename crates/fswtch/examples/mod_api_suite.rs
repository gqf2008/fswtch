use fswtch::{ModuleBuilder, SUCCESS, Status, sys};

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

// SAFETY: FreeSWITCH calls this function during module load with loader-owned pointers.
unsafe extern "C" fn switch_module_load(
    module_interface: *mut *mut sys::switch_loadable_module_interface_t,
    pool: *mut sys::switch_memory_pool_t,
) -> Status {
    fswtch::log_info("mod_api_suite", "loading module");
    match ModuleBuilder::new(module_interface, pool, c"mod_api_suite")
        .and_then(|module| module.api(c"rust_ping", c"returns pong", c"rust_ping", ping_api))
        .and_then(|module| {
            module.api(
                c"rust_echo",
                c"echoes the command argument",
                c"rust_echo <text>",
                echo_api,
            )
        })
        .and_then(|module| {
            module.api(
                c"rust_upper",
                c"uppercases the command argument",
                c"rust_upper <text>",
                upper_api,
            )
        }) {
        Ok(_) => SUCCESS,
        Err(error) => error.0,
    }
}
