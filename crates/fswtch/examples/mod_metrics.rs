use std::{
    collections::HashMap,
    ffi::{CStr, c_char},
    sync::{LazyLock, Mutex},
};

use fswtch::{FALSE, Module, SUCCESS, Status, Stream, sys};

static METRICS: LazyLock<Mutex<HashMap<String, u64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
const MAX_METRICS: usize = 1024;

fswtch::module_exports! {
    module = mod_metrics,
    load = switch_module_load,
}

// SAFETY: FreeSWITCH calls this function with pointers matching `switch_api_function_t`.
unsafe extern "C" fn hit_api(
    cmd: *const c_char,
    _session: *mut sys::switch_core_session_t,
    stream: *mut sys::switch_stream_handle_t,
) -> Status {
    fswtch::log_info("mod_metrics", "rust_metrics_hit invoked");
    let Some(name) = command_text(cmd) else {
        let status = write_response(stream, "usage: rust_metrics_hit <name>\n");
        return if status == SUCCESS { FALSE } else { status };
    };

    let mut metrics = METRICS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let key = metric_key(&name);
    if !metrics.contains_key(&key) && metrics.len() >= MAX_METRICS {
        fswtch::log_error("mod_metrics", "metric cardinality limit reached");
        return write_response(stream, "metric cardinality limit reached\n");
    }
    let count = metrics.entry(key.clone()).or_default();
    *count += 1;
    fswtch::log_info(
        "mod_metrics",
        format!("incremented metric={key} count={count}"),
    );
    write_response(stream, &format!("metric={key} count={count}\n"))
}

// SAFETY: FreeSWITCH calls this function with pointers matching `switch_api_function_t`.
unsafe extern "C" fn show_api(
    _cmd: *const c_char,
    _session: *mut sys::switch_core_session_t,
    stream: *mut sys::switch_stream_handle_t,
) -> Status {
    fswtch::log_info("mod_metrics", "rust_metrics_show invoked");
    let metrics = METRICS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut lines = String::from(
        "# HELP fswtch_example_events_total Example module event counter\n# TYPE fswtch_example_events_total counter\n",
    );
    for (name, count) in metrics.iter() {
        lines.push_str(&format!(
            "fswtch_example_events_total{{name=\"{name}\"}} {count}\n"
        ));
    }
    write_response(stream, &lines)
}

// SAFETY: FreeSWITCH calls this function during module load with loader-owned pointers.
unsafe extern "C" fn switch_module_load(
    module_interface: *mut *mut sys::switch_loadable_module_interface_t,
    pool: *mut sys::switch_memory_pool_t,
) -> Status {
    fswtch::log_info("mod_metrics", "loading module");
    let module = match Module::create(module_interface, pool, c"mod_metrics") {
        Ok(module) => module,
        Err(error) => return error.0,
    };

    for result in [
        module.add_api(
            c"rust_metrics_hit",
            c"increments a named example counter",
            c"rust_metrics_hit <name>",
            hit_api,
        ),
        module.add_api(
            c"rust_metrics_show",
            c"prints example counters in Prometheus text format",
            c"rust_metrics_show",
            show_api,
        ),
    ] {
        if let Err(error) = result {
            return error.0;
        }
    }

    SUCCESS
}

fn metric_key(name: &str) -> String {
    name.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn command_text(cmd: *const c_char) -> Option<String> {
    if cmd.is_null() {
        return None;
    }

    // SAFETY: FreeSWITCH passes a null-terminated command string when one is present.
    unsafe { CStr::from_ptr(cmd) }
        .to_str()
        .ok()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}

fn write_response(stream: *mut sys::switch_stream_handle_t, text: &str) -> Status {
    // SAFETY: FreeSWITCH provides a valid stream pointer for the duration of the API callback.
    let Some(mut stream) = Stream::from_raw(stream) else {
        return FALSE;
    };

    match stream.write_str(text) {
        Ok(()) => SUCCESS,
        Err(error) => error.0,
    }
}
