fswtch::module_exports! {
    module = mod_hello,
    load = switch_module_load,
}

fswtch::api_callback! {
    fn hello_api(_cmd, _session, stream) {
        fswtch::log_info("mod_hello", "rust_hello invoked");
        let Some(stream) = stream else {
            return fswtch::FALSE;
        };
        stream.write("hello from Rust\n")
    }
}

fswtch::module_load! {
    fn switch_module_load(module) for "mod_hello" {
        fswtch::log_info("mod_hello", "loading module");
        module.api(
            "rust_hello",
            "prints a Rust greeting",
            "rust_hello",
            hello_api,
        )
    }
}
