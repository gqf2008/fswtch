use std::{
    ffi::{CStr, CString, c_char},
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
};

use fswtch::{FALSE, Module, SUCCESS, Status, Stream, sys};
use serde_json::{Value, json};

static CDRS_ENRICHED: AtomicUsize = AtomicUsize::new(0);

fswtch::module_exports! {
    module = mod_cdr_enricher,
    load = switch_module_load,
}

#[derive(Debug, Clone)]
struct EnrichedCdr {
    uuid: String,
    account: String,
    tier: &'static str,
    risk: u8,
    billable_seconds: u64,
}

// SAFETY: FreeSWITCH calls this function with pointers matching `switch_api_function_t`.
unsafe extern "C" fn enrich_api(
    cmd: *const c_char,
    _session: *mut sys::switch_core_session_t,
    stream: *mut sys::switch_stream_handle_t,
) -> Status {
    fswtch::log_info("mod_cdr_enricher", "rust_cdr_enrich invoked");
    let Some(text) = command_text(cmd) else {
        fswtch::log_info("mod_cdr_enricher", "missing CDR JSON");
        let status = write_response(stream, "usage: rust_cdr_enrich <json-cdr>\n");
        return if status == SUCCESS { FALSE } else { status };
    };

    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        fswtch::log_info("mod_cdr_enricher", "invalid CDR JSON");
        let status = write_response(stream, "invalid cdr json\n");
        return if status == SUCCESS { FALSE } else { status };
    };

    let enriched = enrich_cdr(&value);
    if let Err(error) = fire_cdr_event(&enriched) {
        return error.0;
    }
    fswtch::log_info(
        "mod_cdr_enricher",
        format!("enriched CDR uuid={} tier={}", enriched.uuid, enriched.tier),
    );
    CDRS_ENRICHED.fetch_add(1, Ordering::Relaxed);

    let response = json!({
        "uuid": enriched.uuid,
        "account": enriched.account,
        "account_tier": enriched.tier,
        "risk_score": enriched.risk,
        "billable_seconds": enriched.billable_seconds,
    });
    write_response(stream, &format!("{response}\n"))
}

// SAFETY: FreeSWITCH calls this function with pointers matching `switch_api_function_t`.
unsafe extern "C" fn stats_api(
    _cmd: *const c_char,
    _session: *mut sys::switch_core_session_t,
    stream: *mut sys::switch_stream_handle_t,
) -> Status {
    fswtch::log_info("mod_cdr_enricher", "rust_cdr_enricher_stats invoked");
    write_response(
        stream,
        &format!("cdrs_enriched={}\n", CDRS_ENRICHED.load(Ordering::Relaxed)),
    )
}

// SAFETY: FreeSWITCH calls this function during module load with loader-owned pointers.
unsafe extern "C" fn switch_module_load(
    module_interface: *mut *mut sys::switch_loadable_module_interface_t,
    pool: *mut sys::switch_memory_pool_t,
) -> Status {
    fswtch::log_info("mod_cdr_enricher", "loading module");
    let module = match Module::create(module_interface, pool, c"mod_cdr_enricher") {
        Ok(module) => module,
        Err(error) => return error.0,
    };

    for result in [
        module.add_api(
            c"rust_cdr_enrich",
            c"enriches a CDR JSON document and emits a custom event",
            c"rust_cdr_enrich <json-cdr>",
            enrich_api,
        ),
        module.add_api(
            c"rust_cdr_enricher_stats",
            c"prints CDR enrichment counters",
            c"rust_cdr_enricher_stats",
            stats_api,
        ),
    ] {
        if let Err(error) = result {
            return error.0;
        }
    }

    SUCCESS
}

fn enrich_cdr(cdr: &Value) -> EnrichedCdr {
    let uuid = cdr
        .get("uuid")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    let account = cdr
        .get("account")
        .and_then(Value::as_str)
        .unwrap_or("anonymous")
        .to_owned();
    let destination = cdr
        .get("destination")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let duration = cdr
        .get("duration")
        .and_then(Value::as_u64)
        .unwrap_or_default();

    let tier = if account.starts_with("vip") {
        "gold"
    } else if account == "anonymous" {
        "guest"
    } else {
        "standard"
    };
    let risk = if destination.starts_with("+882") || destination.starts_with("+979") {
        95
    } else if duration > 3600 {
        70
    } else {
        12
    };

    EnrichedCdr {
        uuid,
        account,
        tier,
        risk,
        billable_seconds: duration.div_ceil(6) * 6,
    }
}

fn fire_cdr_event(cdr: &EnrichedCdr) -> fswtch::Result<()> {
    let mut event = ptr::null_mut();
    // SAFETY: FreeSWITCH initializes `event` for the custom subclass when the call succeeds.
    let status = unsafe {
        sys::switch_event_create_subclass_detailed(
            c"mod_cdr_enricher.rs".as_ptr(),
            c"fire_cdr_event".as_ptr(),
            line!() as _,
            &mut event,
            sys::switch_event_types_t::SWITCH_EVENT_CUSTOM,
            c"fswtch::cdr_enriched".as_ptr(),
        )
    };
    fswtch::status_to_result(status)?;

    add_event_header(event, c"CDR-UUID", &cdr.uuid)?;
    add_event_header(event, c"CDR-Account", &cdr.account)?;
    add_event_header(event, c"CDR-Account-Tier", cdr.tier)?;
    add_event_header(event, c"CDR-Risk-Score", &cdr.risk.to_string())?;
    add_event_header(
        event,
        c"CDR-Billable-Seconds",
        &cdr.billable_seconds.to_string(),
    )?;

    // SAFETY: `event` was created above and ownership is transferred to FreeSWITCH on success.
    let status = unsafe {
        sys::switch_event_fire_detailed(
            c"mod_cdr_enricher.rs".as_ptr(),
            c"fire_cdr_event".as_ptr(),
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
    // SAFETY: `event` is live and the C strings are valid for this call.
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
