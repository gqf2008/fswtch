use std::{
    ffi::{CStr, CString, c_char},
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
};

use fswtch::{FALSE, Module, SUCCESS, Status, Stream, sys};

static MESSAGES_BRIDGED: AtomicUsize = AtomicUsize::new(0);

fswtch::module_exports! {
    module = mod_chatbot_bridge,
    load = switch_module_load,
}

// SAFETY: FreeSWITCH calls this function with pointers matching `switch_chat_application_function_t`.
unsafe extern "C" fn chatbot_app(event: *mut sys::switch_event_t, data: *const c_char) -> Status {
    fswtch::log_info("mod_chatbot_bridge", "chat application invoked");
    let text = command_text(data).unwrap_or_else(|| "empty chat payload".to_owned());
    let from = event_header(event, c"from").unwrap_or_else(|| "unknown".to_owned());
    let to = event_header(event, c"to").unwrap_or_else(|| "unknown".to_owned());

    let mut out = ptr::null_mut();
    // SAFETY: FreeSWITCH initializes `out` when the call succeeds.
    let status = unsafe {
        sys::switch_event_create_subclass_detailed(
            c"mod_chatbot_bridge.rs".as_ptr(),
            c"chatbot_app".as_ptr(),
            line!() as _,
            &mut out,
            sys::switch_event_types_t::SWITCH_EVENT_CUSTOM,
            c"fswtch::chatbot_bridge".as_ptr(),
        )
    };
    if status != SUCCESS {
        return status;
    }

    for result in [
        add_event_header(out, c"Chatbot-From", &from),
        add_event_header(out, c"Chatbot-To", &to),
        add_event_header(out, c"Chatbot-Text", &text),
        add_event_header(out, c"Chatbot-Provider", "example-llm"),
    ] {
        if let Err(error) = result {
            return error.0;
        }
    }

    // SAFETY: `out` was created above and ownership transfers to FreeSWITCH on success.
    let status = unsafe {
        sys::switch_event_fire_detailed(
            c"mod_chatbot_bridge.rs".as_ptr(),
            c"chatbot_app".as_ptr(),
            line!() as _,
            &mut out,
            ptr::null_mut(),
        )
    };
    if status != SUCCESS {
        return status;
    }
    MESSAGES_BRIDGED.fetch_add(1, Ordering::Relaxed);
    fswtch::log_info(
        "mod_chatbot_bridge",
        format!("bridged chat message from={from} to={to}"),
    );

    SUCCESS
}

// SAFETY: FreeSWITCH calls this function with pointers matching `switch_api_function_t`.
unsafe extern "C" fn stats_api(
    _cmd: *const c_char,
    _session: *mut sys::switch_core_session_t,
    stream: *mut sys::switch_stream_handle_t,
) -> Status {
    fswtch::log_info("mod_chatbot_bridge", "rust_chatbot_bridge_stats invoked");
    write_response(
        stream,
        &format!(
            "chatbot_bridge_registered=true messages_bridged={}\n",
            MESSAGES_BRIDGED.load(Ordering::Relaxed)
        ),
    )
}

// SAFETY: FreeSWITCH calls this function during module load with loader-owned pointers.
unsafe extern "C" fn switch_module_load(
    module_interface: *mut *mut sys::switch_loadable_module_interface_t,
    pool: *mut sys::switch_memory_pool_t,
) -> Status {
    fswtch::log_info("mod_chatbot_bridge", "loading module");
    let module = match Module::create(module_interface, pool, c"mod_chatbot_bridge") {
        Ok(module) => module,
        Err(error) => return error.0,
    };

    if let Err(error) = module.add_chat_application(
        c"rust_chatbot_bridge",
        c"Transforms inbound chat messages into custom chatbot events",
        c"Rust chatbot bridge example",
        c"rust_chatbot_bridge <message>",
        chatbot_app,
    ) {
        return error.0;
    }

    if let Err(error) = module.add_api(
        c"rust_chatbot_bridge_stats",
        c"prints chatbot bridge counters",
        c"rust_chatbot_bridge_stats",
        stats_api,
    ) {
        return error.0;
    }

    SUCCESS
}

fn event_header(event: *mut sys::switch_event_t, name: &'static CStr) -> Option<String> {
    if event.is_null() {
        return None;
    }

    // SAFETY: `event` is a live FreeSWITCH event for the chat callback.
    let value = unsafe { sys::switch_event_get_header_idx(event, name.as_ptr(), -1) };
    if value.is_null() {
        return None;
    }

    // SAFETY: FreeSWITCH returns a null-terminated header value when present.
    unsafe { CStr::from_ptr(value) }
        .to_str()
        .ok()
        .map(ToOwned::to_owned)
}

fn add_event_header(
    event: *mut sys::switch_event_t,
    name: &'static CStr,
    value: &str,
) -> fswtch::Result<()> {
    let value = CString::new(value).map_err(|_| fswtch::SwitchError(fswtch::GENERR))?;
    // SAFETY: `event` is live and the C strings are valid for the duration of this call.
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
