use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::Value;

static EVENTS_FIRED: AtomicUsize = AtomicUsize::new(0);

fswtch::module_exports! {
    module = mod_event_sink,
    load = switch_module_load,
}

fswtch::api_callback! {
    fn emit_api(cmd, _session, stream) {
        fswtch::log_info("mod_event_sink", "fswtch_event_sink_emit invoked");
        let Some(stream) = stream else {
            return fswtch::FALSE;
        };
        let Some(request) = cmd.as_deref().and_then(EventRequest::parse) else {
            fswtch::log_info("mod_event_sink", "invalid event sink command");
            let status = stream.write("usage: fswtch_event_sink_emit <subclass> <json-object>\n");
            return fswtch::false_on_success(status);
        };

        match fire_event(&request) {
            Ok(()) => {
                let count = EVENTS_FIRED.fetch_add(1, Ordering::Relaxed) + 1;
                fswtch::log_info(
                    "mod_event_sink",
                    format!("fired event subclass={} count={count}", request.subclass),
                );
                stream.write(&format!("event fired count={count}\n"))
            }
            Err(error) => error.status(),
        }
    }
}

fswtch::api_callback! {
    fn stats_api(_cmd, _session, stream) {
        fswtch::log_info("mod_event_sink", "fswtch_event_sink_stats invoked");
        let Some(stream) = stream else {
            return fswtch::FALSE;
        };
        stream.write(
            &format!("events_fired={}\n", EVENTS_FIRED.load(Ordering::Relaxed)),
        )
    }
}

fswtch::module_load! {
    fn switch_module_load(module) for "mod_event_sink" {
        fswtch::log_info("mod_event_sink", "loading module");
        module
            .api(
                "fswtch_event_sink_emit",
                "fires a custom event from a JSON object",
                "fswtch_event_sink_emit <subclass> <json-object>",
                emit_api,
            )
            .and_then(|module| {
                module.api(
                    "fswtch_event_sink_stats",
                    "prints event sink counters",
                    "fswtch_event_sink_stats",
                    stats_api,
                )
            })
    }
}

#[derive(Debug)]
struct EventRequest {
    subclass: String,
    headers: Vec<(String, String)>,
}

impl EventRequest {
    fn parse(text: &str) -> Option<Self> {
        let (subclass, json) = text.split_once(char::is_whitespace)?;
        let Value::Object(object) = serde_json::from_str(json.trim()).ok()? else {
            return None;
        };

        let headers = object
            .into_iter()
            .map(|(name, value)| {
                let value = match value {
                    Value::String(text) => text,
                    other => other.to_string(),
                };
                (format!("Rust-{}", header_case(&name)), value)
            })
            .collect();

        Some(Self {
            subclass: subclass.to_owned(),
            headers,
        })
    }
}

fn fire_event(request: &EventRequest) -> fswtch::Result<()> {
    let mut event = fswtch::Event::custom(&request.subclass)?;

    for (name, value) in &request.headers {
        event.add_header_name(name, value)?;
    }

    event.fire()
}

fn header_case(name: &str) -> String {
    name.split(['_', '-'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join("-")
}
