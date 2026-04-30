use fswtch::{ModuleBuilder, SUCCESS, Status, sys};

fswtch::module_exports! {
    module = mod_hello,
    load = switch_module_load,
}

fswtch::api_callback! {
    fn hello_api(_cmd, _session, stream) {
        fswtch::log_info("mod_hello", "rust_hello invoked");
        stream.write("hello from Rust\n")
    }
}

// SAFETY: FreeSWITCH calls this function during module load with loader-owned pointers.
unsafe extern "C" fn switch_module_load(
    module_interface: *mut *mut sys::switch_loadable_module_interface_t,
    pool: *mut sys::switch_memory_pool_t,
) -> Status {
    fswtch::log_info("mod_hello", "loading module");
    match ModuleBuilder::new(module_interface, pool, c"mod_hello").and_then(|module| {
        module.api(
            c"rust_hello",
            c"prints a Rust greeting",
            c"rust_hello",
            hello_api,
        )
    }) {
        Ok(_) => SUCCESS,
        Err(error) => error.0,
    }
}
