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

fswtch::module_load! {
    fn switch_module_load(module) for c"mod_hello" {
        fswtch::log_info("mod_hello", "loading module");
        module.api(
            c"rust_hello",
            c"prints a Rust greeting",
            c"rust_hello",
            hello_api,
        )
    }
}
