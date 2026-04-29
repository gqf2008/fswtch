use std::{
    ffi::{CStr, CString, c_char},
    ptr, thread,
    time::Duration,
};

use fswtch::{FALSE, Module, SUCCESS, Status, Stream, sys};
use serde_json::Value;

fswtch::module_exports! {
    module = mod_registration_check,
    load = switch_module_load,
}

unsafe extern "C" fn check_registration_api(
    cmd: *const c_char,
    _session: *mut sys::switch_core_session_t,
    stream: *mut sys::switch_stream_handle_t,
) -> Status {
    let Some(request) = RegistrationRequest::parse(cmd) else {
        write_response(
            stream,
            "usage: rust_check_registration <user@domain> <https://server/check>\n",
        );
        return FALSE;
    };

    write_response(stream, "registration check queued\n");

    thread::spawn(move || {
        let result = check_registration_remotely(&request);
        let _ = fire_registration_event(&request, &result);
    });

    SUCCESS
}

unsafe extern "C" fn switch_module_load(
    module_interface: *mut *mut sys::switch_loadable_module_interface_t,
    pool: *mut sys::switch_memory_pool_t,
) -> Status {
    let module = match unsafe { Module::create(module_interface, pool, c"mod_registration_check") }
    {
        Ok(module) => module,
        Err(error) => return error.0,
    };

    if let Err(error) = unsafe {
        module.add_api(
            c"rust_check_registration",
            c"asynchronously validates a registration and fires a custom event",
            c"rust_check_registration <user@domain> <https://server/check>",
            check_registration_api,
        )
    } {
        return error.0;
    }

    SUCCESS
}

#[derive(Debug, Clone)]
struct RegistrationRequest {
    user: String,
    server_url: String,
}

impl RegistrationRequest {
    fn parse(cmd: *const c_char) -> Option<Self> {
        let text = command_text(cmd)?;
        let mut fields = text.split_whitespace();
        let user = fields.next()?.to_owned();
        let server_url = fields.next()?.to_owned();

        Some(Self { user, server_url })
    }
}

#[derive(Debug, Clone)]
struct RegistrationResult {
    accepted: bool,
    score: u8,
    reason: String,
    request_id: String,
}

fn check_registration_remotely(request: &RegistrationRequest) -> RegistrationResult {
    thread::sleep(Duration::from_millis(150));

    let pretend_json = if request.user.ends_with("@blocked.example") {
        r#"{"accepted":false,"score":15,"reason":"blocked_domain","request_id":"reg-1002"}"#
    } else {
        r#"{"accepted":true,"score":94,"reason":"ok","request_id":"reg-1001"}"#
    };

    parse_registration_json(pretend_json)
}

fn parse_registration_json(json: &str) -> RegistrationResult {
    let json: Value = serde_json::from_str(json).unwrap_or(Value::Null);

    RegistrationResult {
        accepted: json
            .get("accepted")
            .and_then(Value::as_bool)
            .unwrap_or_default(),
        score: json
            .get("score")
            .and_then(Value::as_u64)
            .and_then(|score| u8::try_from(score).ok())
            .unwrap_or_default(),
        reason: json
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned(),
        request_id: json
            .get("request_id")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned(),
    }
}

fn fire_registration_event(
    request: &RegistrationRequest,
    result: &RegistrationResult,
) -> fswtch::Result<()> {
    let mut event = ptr::null_mut();
    let status = unsafe {
        sys::switch_event_create_subclass_detailed(
            c"mod_registration_check.rs".as_ptr(),
            c"fire_registration_event".as_ptr(),
            line!() as _,
            &mut event,
            sys::switch_event_types_t::SWITCH_EVENT_CUSTOM,
            c"fswtch::registration_check".as_ptr(),
        )
    };
    fswtch::status_to_result(status)?;

    add_event_header(event, c"Registration-User", &request.user)?;
    add_event_header(event, c"Registration-Server", &request.server_url)?;
    add_event_header(
        event,
        c"Registration-Accepted",
        if result.accepted { "true" } else { "false" },
    )?;
    add_event_header(event, c"Registration-Score", &result.score.to_string())?;
    add_event_header(event, c"Registration-Reason", &result.reason)?;
    add_event_header(event, c"Registration-Request-ID", &result.request_id)?;

    let status = unsafe {
        sys::switch_event_fire_detailed(
            c"mod_registration_check.rs".as_ptr(),
            c"fire_registration_event".as_ptr(),
            line!() as _,
            &mut event,
            ptr::null_mut(),
        )
    };
    fswtch::status_to_result(status)
}

fn add_event_header(
    event: *mut sys::switch_event_t,
    name: &'static CStr,
    value: &str,
) -> fswtch::Result<()> {
    let value = CString::new(value).map_err(|_| fswtch::SwitchError(fswtch::GENERR))?;
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

    unsafe { CStr::from_ptr(cmd) }
        .to_str()
        .ok()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}

fn write_response(stream: *mut sys::switch_stream_handle_t, text: &str) {
    if let Some(mut stream) = unsafe { Stream::from_raw(stream) } {
        let _ = stream.write_str(text);
    }
}
