fswtch::module_exports! {
    module = mod_stream_tools,
    load = switch_module_load,
}

fswtch::api_callback! {
    fn table_api(_cmd, _session, stream) {
        fswtch::log_info("mod_stream_tools", "rust_table invoked");
        let Some(stream) = stream else {
            return fswtch::FALSE;
        };
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
        let Some(stream) = stream else {
            return fswtch::FALSE;
        };

        let Some(text) = cmd else {
            return stream.write("0 words\n");
        };

        let count = text.split_whitespace().count();
        stream.write(&format!("{count} words\n"))
    }
}

fswtch::module_load! {
    fn switch_module_load(module) for "mod_stream_tools" {
        fswtch::log_info("mod_stream_tools", "loading module");
        module
            .api(
            "rust_table",
            "prints a small CSV response",
            "rust_table",
            table_api,
            )
            .and_then(|module| {
                module.api(
                    "rust_words",
                    "counts words in the command argument",
                    "rust_words <text>",
                    words_api,
                )
            })
    }
}
