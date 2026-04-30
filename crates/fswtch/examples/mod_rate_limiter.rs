use std::{
    collections::HashMap,
    ffi::{CStr, c_char},
    sync::{LazyLock, Mutex},
    time::{Duration, Instant},
};

use fswtch::{FALSE, Module, SUCCESS, Status, Stream, sys};

static LIMITERS: LazyLock<Mutex<HashMap<String, Bucket>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
const MAX_BUCKETS: usize = 10_000;

fswtch::module_exports! {
    module = mod_rate_limiter,
    load = switch_module_load,
}

#[derive(Debug, Clone)]
struct Bucket {
    remaining: u32,
    reset_at: Instant,
}

#[derive(Debug, Clone)]
struct LimitRequest {
    key: String,
    limit: u32,
    window: Duration,
}

impl LimitRequest {
    fn parse(cmd: *const c_char) -> Option<Self> {
        let text = command_text(cmd)?;
        let mut parts = text.split_whitespace();
        let key = parts.next()?.to_owned();
        let limit = parts
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(10);
        let window_secs = parts
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(60);
        Some(Self {
            key,
            limit,
            window: Duration::from_secs(window_secs),
        })
    }
}

// SAFETY: FreeSWITCH calls this function with pointers matching `switch_api_function_t`.
unsafe extern "C" fn allow_api(
    cmd: *const c_char,
    _session: *mut sys::switch_core_session_t,
    stream: *mut sys::switch_stream_handle_t,
) -> Status {
    fswtch::log_example("mod_rate_limiter", "rust_rate_limit invoked");
    let Some(request) = LimitRequest::parse(cmd) else {
        let status = write_response(
            stream,
            "usage: rust_rate_limit <key> [limit] [window-secs]\n",
        );
        return if status == SUCCESS { FALSE } else { status };
    };

    let now = Instant::now();
    let mut limiters = LIMITERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !limiters.contains_key(&request.key) && limiters.len() >= MAX_BUCKETS {
        fswtch::log_example_error("mod_rate_limiter", "rate limiter bucket limit reached");
        return write_response(stream, "rate limiter bucket limit reached\n");
    }
    let bucket = limiters
        .entry(request.key.clone())
        .or_insert_with(|| Bucket {
            remaining: request.limit,
            reset_at: now + request.window,
        });

    if now >= bucket.reset_at {
        bucket.remaining = request.limit;
        bucket.reset_at = now + request.window;
    }

    let allowed = bucket.remaining > 0;
    if allowed {
        bucket.remaining -= 1;
    }
    fswtch::log_example(
        "mod_rate_limiter",
        format!(
            "key={} allowed={} remaining={}",
            request.key, allowed, bucket.remaining
        ),
    );

    write_response(
        stream,
        &format!(
            "key={} allowed={} remaining={}\n",
            request.key, allowed, bucket.remaining
        ),
    )
}

// SAFETY: FreeSWITCH calls this function with pointers matching `switch_api_function_t`.
unsafe extern "C" fn reset_api(
    _cmd: *const c_char,
    _session: *mut sys::switch_core_session_t,
    stream: *mut sys::switch_stream_handle_t,
) -> Status {
    fswtch::log_example("mod_rate_limiter", "rust_rate_limit_reset invoked");
    LIMITERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
    write_response(stream, "rate limiters reset\n")
}

// SAFETY: FreeSWITCH calls this function during module load with loader-owned pointers.
unsafe extern "C" fn switch_module_load(
    module_interface: *mut *mut sys::switch_loadable_module_interface_t,
    pool: *mut sys::switch_memory_pool_t,
) -> Status {
    fswtch::log_example("mod_rate_limiter", "loading module");
    // SAFETY: The loader passes the module slot and pool, and the module name is static.
    let module = match unsafe { Module::create(module_interface, pool, c"mod_rate_limiter") } {
        Ok(module) => module,
        Err(error) => return error.0,
    };

    for result in [
        // SAFETY: The callback and C strings remain valid for the loaded module lifetime.
        unsafe {
            module.add_api(
                c"rust_rate_limit",
                c"checks a token-bucket rate limit",
                c"rust_rate_limit <key> [limit] [window-secs]",
                allow_api,
            )
        },
        // SAFETY: The callback and C strings remain valid for the loaded module lifetime.
        unsafe {
            module.add_api(
                c"rust_rate_limit_reset",
                c"clears all rate limiter buckets",
                c"rust_rate_limit_reset",
                reset_api,
            )
        },
    ] {
        if let Err(error) = result {
            return error.0;
        }
    }

    SUCCESS
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
    let Some(mut stream) = (unsafe { Stream::from_raw(stream) }) else {
        return FALSE;
    };

    match stream.write_str(text) {
        Ok(()) => SUCCESS,
        Err(error) => error.0,
    }
}
