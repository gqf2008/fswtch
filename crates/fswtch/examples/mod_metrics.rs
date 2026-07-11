use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
};

static METRICS: LazyLock<Mutex<HashMap<String, u64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
const MAX_METRICS: usize = 1024;

fswtch::module_exports! {
    module = mod_metrics,
    load = switch_module_load,
}

fswtch::api_callback! {
    fn hit_api(cmd, _session, stream) {
        fswtch::log_info("mod_metrics", "fswtch_metrics_hit invoked");
        let Some(stream) = stream else {
            return fswtch::FALSE;
        };
        let Some(name) = cmd else {
            let status = stream.write("usage: fswtch_metrics_hit <name>\n");
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
        fswtch::log_info("mod_metrics", "fswtch_metrics_show invoked");
        let Some(stream) = stream else {
            return fswtch::FALSE;
        };
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

fswtch::module_load! {
    fn switch_module_load(module) for "mod_metrics" {
        fswtch::log_info("mod_metrics", "loading module");
        module
            .api(
                "fswtch_metrics_hit",
                "increments a named example counter",
                "fswtch_metrics_hit <name>",
                hit_api,
            )
            .and_then(|module| {
                module.api(
                    "fswtch_metrics_show",
                    "prints example counters in Prometheus text format",
                    "fswtch_metrics_show",
                    show_api,
                )
            })
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
