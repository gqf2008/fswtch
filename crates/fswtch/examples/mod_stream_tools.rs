use fswtch::{ModuleBuilder, SUCCESS, Status, sys};

fswtch::module_exports! {
    module = mod_stream_tools,
    load = switch_module_load,
}

fswtch::api_callback! {
    fn table_api(_cmd, _session, stream) {
        fswtch::log_info("mod_stream_tools", "rust_table invoked");
        stream.write(
            &[
                "name,value\n",
                "language,rust\n",
                "module,mod_stream_tools\n",
                "binding,fswtch\n",
            ]
            .concat(),
        )
    }
}

fswtch::api_callback! {
    fn words_api(cmd, _session, stream) {
        fswtch::log_info("mod_stream_tools", "rust_words invoked");

        let Some(text) = cmd else {
            return stream.write("0 words\n");
        };

        let count = text.split_whitespace().count();
        stream.write(&format!("{count} words\n"))
    }
}

// SAFETY: FreeSWITCH calls this function during module load with loader-owned pointers.
unsafe extern "C" fn switch_module_load(
    module_interface: *mut *mut sys::switch_loadable_module_interface_t,
    pool: *mut sys::switch_memory_pool_t,
) -> Status {
    fswtch::log_info("mod_stream_tools", "loading module");
    match ModuleBuilder::new(module_interface, pool, c"mod_stream_tools")
        .and_then(|module| {
            module.api(
                c"rust_table",
                c"prints a small CSV response",
                c"rust_table",
                table_api,
            )
        })
        .and_then(|module| {
            module.api(
                c"rust_words",
                c"counts words in the command argument",
                c"rust_words <text>",
                words_api,
            )
        }) {
        Ok(_) => SUCCESS,
        Err(error) => error.0,
    }
}
