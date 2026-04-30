use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
};

use fswtch::{ModuleBuilder, SUCCESS, Status, sys};

static METRICS: LazyLock<Mutex<HashMap<String, u64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
const MAX_METRICS: usize = 1024;

fswtch::module_exports! {
    module = mod_metrics,
    load = switch_module_load,
}

fswtch::api_callback! {
    fn hit_api(cmd, _session, stream) {
        fswtch::log_info("mod_metrics", "rust_metrics_hit invoked");
        let Some(name) = cmd else {
            let status = stream.write("usage: rust_metrics_hit <name>\n");
            return fswtch::false_on_success(status);
        };

        let mut metrics = METRICS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let key = metric_key(&name);
        if !metrics.contains_key(&key) && metrics.len() >= MAX_METRICS {
            fswtch::log_error("mod_metrics", "metric cardinality limit reached");
            return stream.write("metric cardinality limit reached\n");
        }
        let count = metrics.entry(key.clone()).or_default();
        *count += 1;
        fswtch::log_info(
            "mod_metrics",
            format!("incremented metric={key} count={count}"),
        );
        stream.write(&format!("metric={key} count={count}\n"))
    }
}

fswtch::api_callback! {
    fn show_api(_cmd, _session, stream) {
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
        stream.write(&lines)
    }
}

// SAFETY: FreeSWITCH calls this function during module load with loader-owned pointers.
unsafe extern "C" fn switch_module_load(
    module_interface: *mut *mut sys::switch_loadable_module_interface_t,
    pool: *mut sys::switch_memory_pool_t,
) -> Status {
    fswtch::log_info("mod_metrics", "loading module");
    match ModuleBuilder::new(module_interface, pool, c"mod_metrics")
        .and_then(|module| {
            module.api(
                c"rust_metrics_hit",
                c"increments a named example counter",
                c"rust_metrics_hit <name>",
                hit_api,
            )
        })
        .and_then(|module| {
            module.api(
                c"rust_metrics_show",
                c"prints example counters in Prometheus text format",
                c"rust_metrics_show",
                show_api,
            )
        }) {
        Ok(_) => SUCCESS,
        Err(error) => error.0,
    }
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
