use std::{
    ffi::{CStr, c_char},
    ptr,
    sync::{LazyLock, Mutex},
};

use fswtch::{FALSE, Module, SUCCESS, Status, Stream, sys};

static CONFIG: LazyLock<Mutex<Config>> = LazyLock::new(|| Mutex::new(Config::default()));

fswtch::module_exports! {
    module = mod_config_xml,
    load = switch_module_load,
}

#[derive(Debug, Clone)]
struct Config {
    enabled: bool,
    greeting: String,
    max_sessions: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            greeting: "hello from XML config".to_owned(),
            max_sessions: 8,
        }
    }
}

// SAFETY: FreeSWITCH calls this function with pointers matching `switch_api_function_t`.
unsafe extern "C" fn show_api(
    _cmd: *const c_char,
    _session: *mut sys::switch_core_session_t,
    stream: *mut sys::switch_stream_handle_t,
) -> Status {
    fswtch::log_info("mod_config_xml", "rust_config_xml_show invoked");
    let config = CONFIG
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    write_response(
        stream,
        &format!(
            "enabled={} greeting={} max_sessions={}\n",
            config.enabled, config.greeting, config.max_sessions
        ),
    )
}

// SAFETY: FreeSWITCH calls this function with pointers matching `switch_api_function_t`.
unsafe extern "C" fn reload_api(
    _cmd: *const c_char,
    _session: *mut sys::switch_core_session_t,
    stream: *mut sys::switch_stream_handle_t,
) -> Status {
    fswtch::log_info("mod_config_xml", "rust_config_xml_reload invoked");
    match load_config() {
        Ok(config) => {
            *CONFIG
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = config;
            write_response(stream, "config reloaded\n")
        }
        Err(error) => write_response(stream, &format!("config reload failed: {error}\n")),
    }
}

// SAFETY: FreeSWITCH calls this function during module load with loader-owned pointers.
unsafe extern "C" fn switch_module_load(
    module_interface: *mut *mut sys::switch_loadable_module_interface_t,
    pool: *mut sys::switch_memory_pool_t,
) -> Status {
    fswtch::log_info("mod_config_xml", "loading module");
    if let Ok(config) = load_config() {
        *CONFIG
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = config;
    }

    // SAFETY: The loader passes the module slot and pool, and the module name is static.
    let module = match unsafe { Module::create(module_interface, pool, c"mod_config_xml") } {
        Ok(module) => module,
        Err(error) => return error.0,
    };

    for result in [
        // SAFETY: The callback and C strings remain valid for the loaded module lifetime.
        unsafe {
            module.add_api(
                c"rust_config_xml_show",
                c"prints settings loaded from fswtch_examples.conf",
                c"rust_config_xml_show",
                show_api,
            )
        },
        // SAFETY: The callback and C strings remain valid for the loaded module lifetime.
        unsafe {
            module.add_api(
                c"rust_config_xml_reload",
                c"reloads settings from fswtch_examples.conf",
                c"rust_config_xml_reload",
                reload_api,
            )
        },
    ] {
        if let Err(error) = result {
            return error.0;
        }
    }

    SUCCESS
}

fn load_config() -> Result<Config, &'static str> {
    fswtch::log_info("mod_config_xml", "loading fswtch_examples.conf");
    let mut config = Config::default();
    let mut settings = ptr::null_mut();
    // SAFETY: FreeSWITCH writes the configuration node into `settings` when the file is found.
    let root = unsafe {
        sys::switch_xml_open_cfg(
            c"fswtch_examples.conf".as_ptr(),
            &mut settings,
            ptr::null_mut(),
        )
    };
    if root.is_null() {
        return Err("fswtch_examples.conf not found");
    }

    if !settings.is_null() {
        // SAFETY: `settings` is the live configuration node returned by FreeSWITCH.
        let settings_node = unsafe { sys::switch_xml_child(settings, c"settings".as_ptr()) };
        if !settings_node.is_null() {
            parse_settings(settings_node, &mut config);
        }
    }

    // SAFETY: `root` was returned by FreeSWITCH XML APIs and must be released after traversal.
    unsafe {
        sys::switch_xml_free(root);
    }

    Ok(config)
}

fn parse_settings(settings: sys::switch_xml_t, config: &mut Config) {
    // SAFETY: `settings` is a live XML node, and the child name is a static C string.
    let mut param = unsafe { sys::switch_xml_child(settings, c"param".as_ptr()) };
    while !param.is_null() {
        let Some(name) = xml_attr(param, c"name") else {
            // SAFETY: `param` is live for traversal.
            param = unsafe { (*param).next };
            continue;
        };
        let Some(value) = xml_attr(param, c"value") else {
            // SAFETY: `param` is live for traversal.
            param = unsafe { (*param).next };
            continue;
        };

        match name.as_str() {
            "enabled" => config.enabled = matches!(value.as_str(), "true" | "yes" | "1"),
            "greeting" => config.greeting = value,
            "max-sessions" => {
                if let Ok(parsed) = value.parse() {
                    config.max_sessions = parsed;
                }
            }
            _ => {}
        }

        // SAFETY: `param` is live for traversal.
        param = unsafe { (*param).next };
    }
}

fn xml_attr(node: sys::switch_xml_t, name: &'static CStr) -> Option<String> {
    // SAFETY: `node` is live and `name` is a static C string.
    let value = unsafe { sys::switch_xml_attr(node, name.as_ptr()) };
    if value.is_null() {
        return None;
    }

    // SAFETY: FreeSWITCH returns a null-terminated attribute value when present.
    unsafe { CStr::from_ptr(value) }
        .to_str()
        .ok()
        .map(ToOwned::to_owned)
}

fn write_response(stream: *mut sys::switch_stream_handle_t, text: &str) -> Status {
    // SAFETY: FreeSWITCH provides a valid stream pointer for the duration of the API callback.
    let Some(mut stream) = (unsafe { Stream::from_raw(stream) }) else {
        return FALSE;
    };

    match stream.write_str(text) {
        Ok(()) => SUCCESS,
        Err(error) => error.0,
    }
}
