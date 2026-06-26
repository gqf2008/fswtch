//! `mod_utils_demo` — showcases the fswtch string-utility free functions and the
//! core global helpers in a single API command.
//!
//! Subsystem: utils + core.
//!
//! Load the module (`load mod_utils_demo`) and from fs_cli run:
//!   `rust_utils_demo`
//!
//! The command exercises `fswtch::escape_string`, `fswtch::url_encode`,
//! `fswtch::format_number`, `fswtch::find_end_paren`, and the core helpers
//! `fswtch::get_uuid` / `fswtch::get_variable("hostname")`, writing every
//! result to the stream.

fswtch::module_exports! {
    module = mod_utils_demo,
    load = switch_module_load,
}

fswtch::api_callback! {
    fn utils_demo_api(_cmd, _session, stream) {
        fswtch::log_info("mod_utils_demo", "rust_utils_demo invoked");

        // String-utility free functions (each returns Result<String> / Option).
        let escaped = match fswtch::escape_string("he said \"hi\" & bye") {
            Ok(value) => value,
            Err(error) => format!("<escape_string error: {error}>"),
        };
        let encoded = match fswtch::url_encode("a b&c=d") {
            Ok(value) => value,
            Err(error) => format!("<url_encode error: {error}>"),
        };
        let formatted = match fswtch::format_number(1001) {
            Ok(value) => value,
            Err(error) => format!("<format_number error: {error}>"),
        };
        // find_end_paren returns Option<usize> — the index of the matching '}'.
        let paren_idx = fswtch::find_end_paren("{a}", '{', '}');

        // Core global helpers.
        let uuid = fswtch::get_uuid().unwrap_or_else(|| "<none>".to_owned());
        let hostname = match fswtch::get_variable("hostname") {
            Ok(Some(value)) => value,
            Ok(None) => "<unset>".to_owned(),
            Err(error) => format!("<get_variable error: {error}>"),
        };

        stream.write(&format!(
            "escape_string: {escaped}\n\
             url_encode:    {encoded}\n\
             format_number(1001): {formatted}\n\
             find_end_paren(\"{{a}}\", '{{', '}}'): {paren_idx:?}\n\
             get_uuid:     {uuid}\n\
             get_variable(hostname): {hostname}\n"
        ))
    }
}

fswtch::module_load! {
    fn switch_module_load(module) for "mod_utils_demo" {
        fswtch::log_info("mod_utils_demo", "loading module");
        module.api(
            "rust_utils_demo",
            "exercises the fswtch string-utility and core helper functions",
            "rust_utils_demo",
            utils_demo_api,
        )
    }
}
