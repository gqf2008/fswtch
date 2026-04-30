use std::{
    ffi::{CStr, CString, c_char},
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
};

use fswtch::{FALSE, Module, SUCCESS, Status, Stream, sys};
use serde_json::Value;

static EVENTS_FIRED: AtomicUsize = AtomicUsize::new(0);

fswtch::module_exports! {
    module = mod_event_sink,
    load = switch_module_load,
}

// SAFETY: FreeSWITCH calls this function with pointers matching `switch_api_function_t`.
unsafe extern "C" fn emit_api(
    cmd: *const c_char,
    _session: *mut sys::switch_core_session_t,
    stream: *mut sys::switch_stream_handle_t,
) -> Status {
    fswtch::log_info("mod_event_sink", "rust_event_sink_emit invoked");
    let Some(request) = EventRequest::parse(cmd) else {
        fswtch::log_info("mod_event_sink", "invalid event sink command");
        let status = write_response(
            stream,
            "usage: rust_event_sink_emit <subclass> <json-object>\n",
        );
        return if status == SUCCESS { FALSE } else { status };
    };

    match fire_event(&request) {
        Ok(()) => {
            let count = EVENTS_FIRED.fetch_add(1, Ordering::Relaxed) + 1;
            fswtch::log_info(
                "mod_event_sink",
                format!("fired event subclass={} count={count}", request.subclass),
            );
            write_response(stream, &format!("event fired count={count}\n"))
        }
        Err(error) => error.0,
    }
}

// SAFETY: FreeSWITCH calls this function with pointers matching `switch_api_function_t`.
unsafe extern "C" fn stats_api(
    _cmd: *const c_char,
    _session: *mut sys::switch_core_session_t,
    stream: *mut sys::switch_stream_handle_t,
) -> Status {
    fswtch::log_info("mod_event_sink", "rust_event_sink_stats invoked");
    write_response(
        stream,
        &format!("events_fired={}\n", EVENTS_FIRED.load(Ordering::Relaxed)),
    )
}

// SAFETY: FreeSWITCH calls this function during module load with loader-owned pointers.
unsafe extern "C" fn switch_module_load(
    module_interface: *mut *mut sys::switch_loadable_module_interface_t,
    pool: *mut sys::switch_memory_pool_t,
) -> Status {
    fswtch::log_info("mod_event_sink", "loading module");
    let module = match Module::create(module_interface, pool, c"mod_event_sink") {
        Ok(module) => module,
        Err(error) => return error.0,
    };

    for result in [
        module.add_api(
            c"rust_event_sink_emit",
            c"fires a custom event from a JSON object",
            c"rust_event_sink_emit <subclass> <json-object>",
            emit_api,
        ),
        module.add_api(
            c"rust_event_sink_stats",
            c"prints event sink counters",
            c"rust_event_sink_stats",
            stats_api,
        ),
    ] {
        if let Err(error) = result {
            return error.0;
        }
    }

    SUCCESS
}

#[derive(Debug)]
struct EventRequest {
    subclass: String,
    headers: Vec<(String, String)>,
}

impl EventRequest {
    fn parse(cmd: *const c_char) -> Option<Self> {
        let text = command_text(cmd)?;
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
    let subclass =
        CString::new(request.subclass.as_str()).map_err(|_| fswtch::SwitchError(fswtch::GENERR))?;
    let mut event = ptr::null_mut();
    // SAFETY: FreeSWITCH initializes `event` for the custom subclass when the call succeeds.
    let status = unsafe {
        sys::switch_event_create_subclass_detailed(
            c"mod_event_sink.rs".as_ptr(),
            c"fire_event".as_ptr(),
            line!() as _,
            &mut event,
            sys::switch_event_types_t::SWITCH_EVENT_CUSTOM,
            subclass.as_ptr(),
        )
    };
    fswtch::status_to_result(status)?;

    for (name, value) in &request.headers {
        add_event_header(event, name, value)?;
    }

    // SAFETY: `event` was created above and ownership is transferred to FreeSWITCH on success.
    let status = unsafe {
        sys::switch_event_fire_detailed(
            c"mod_event_sink.rs".as_ptr(),
            c"fire_event".as_ptr(),
            line!() as _,
            &mut event,
            ptr::null_mut(),
        )
    };
    fswtch::status_to_result(status)
}

fn add_event_header(
    event: *mut sys::switch_event_t,
    name: &str,
    value: &str,
) -> fswtch::Result<()> {
    let name = CString::new(name).map_err(|_| fswtch::SwitchError(fswtch::GENERR))?;
    let value = CString::new(value).map_err(|_| fswtch::SwitchError(fswtch::GENERR))?;
    // SAFETY: `event` is live and the header/value C strings are valid for this call.
    let status = unsafe {
        sys::switch_event_add_header_string(
            event,
            sys::switch_stack_t::SWITCH_STACK_BOTTOM,
            name.as_ptr(),
            value.as_ptr(),
        )
    };
    fswtch::status_to_result(status)
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
