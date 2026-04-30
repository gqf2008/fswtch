use std::{
    ffi::{CStr, c_char},
    io::{Read, Write},
    net::TcpStream,
    sync::atomic::{AtomicUsize, Ordering},
    thread,
    time::Duration,
};

use fswtch::{FALSE, Module, SUCCESS, Status, Stream, sys};

static WEBHOOKS_QUEUED: AtomicUsize = AtomicUsize::new(0);
static WEBHOOKS_SENT: AtomicUsize = AtomicUsize::new(0);
static WEBHOOKS_FAILED: AtomicUsize = AtomicUsize::new(0);

fswtch::module_exports! {
    module = mod_http_webhook,
    load = switch_module_load,
}

#[derive(Debug, Clone)]
struct WebhookRequest {
    url: HttpUrl,
    body: String,
}

#[derive(Debug, Clone)]
struct HttpUrl {
    host: String,
    port: u16,
    path: String,
}

impl WebhookRequest {
    fn parse(cmd: *const c_char) -> Option<Self> {
        let text = command_text(cmd)?;
        let (url, body) = text.split_once(char::is_whitespace)?;
        Some(Self {
            url: HttpUrl::parse(url)?,
            body: body.trim().to_owned(),
        })
    }
}

impl HttpUrl {
    fn parse(url: &str) -> Option<Self> {
        let rest = url.strip_prefix("http://")?;
        let (authority, path) = match rest.split_once('/') {
            Some((authority, path)) => (authority, format!("/{path}")),
            None => (rest, "/".to_owned()),
        };
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => (host.to_owned(), port.parse().ok()?),
            None => (authority.to_owned(), 80),
        };
        if host.is_empty() {
            return None;
        }
        Some(Self { host, port, path })
    }
}

// SAFETY: FreeSWITCH calls this function with pointers matching `switch_api_function_t`.
unsafe extern "C" fn post_api(
    cmd: *const c_char,
    _session: *mut sys::switch_core_session_t,
    stream: *mut sys::switch_stream_handle_t,
) -> Status {
    let Some(request) = WebhookRequest::parse(cmd) else {
        let status = write_response(stream, "usage: rust_webhook_post <http-url> <json-body>\n");
        return if status == SUCCESS { FALSE } else { status };
    };

    WEBHOOKS_QUEUED.fetch_add(1, Ordering::Relaxed);
    let worker = thread::Builder::new()
        .name("fswtch-http-webhook".to_owned())
        .spawn(move || match post_webhook(&request) {
            Ok(()) => {
                WEBHOOKS_SENT.fetch_add(1, Ordering::Relaxed);
            }
            Err(error) => {
                WEBHOOKS_FAILED.fetch_add(1, Ordering::Relaxed);
                eprintln!("webhook delivery failed: {error}");
            }
        });
    if worker.is_err() {
        return fswtch::GENERR;
    }

    write_response(stream, "webhook queued\n")
}

// SAFETY: FreeSWITCH calls this function with pointers matching `switch_api_function_t`.
unsafe extern "C" fn stats_api(
    _cmd: *const c_char,
    _session: *mut sys::switch_core_session_t,
    stream: *mut sys::switch_stream_handle_t,
) -> Status {
    write_response(
        stream,
        &format!(
            "queued={} sent={} failed={}\n",
            WEBHOOKS_QUEUED.load(Ordering::Relaxed),
            WEBHOOKS_SENT.load(Ordering::Relaxed),
            WEBHOOKS_FAILED.load(Ordering::Relaxed)
        ),
    )
}

// SAFETY: FreeSWITCH calls this function during module load with loader-owned pointers.
unsafe extern "C" fn switch_module_load(
    module_interface: *mut *mut sys::switch_loadable_module_interface_t,
    pool: *mut sys::switch_memory_pool_t,
) -> Status {
    // SAFETY: The loader passes the module slot and pool, and the module name is static.
    let module = match unsafe { Module::create(module_interface, pool, c"mod_http_webhook") } {
        Ok(module) => module,
        Err(error) => return error.0,
    };

    for result in [
        // SAFETY: The callback and C strings remain valid for the loaded module lifetime.
        unsafe {
            module.add_api(
                c"rust_webhook_post",
                c"queues a plain HTTP webhook POST",
                c"rust_webhook_post <http-url> <json-body>",
                post_api,
            )
        },
        // SAFETY: The callback and C strings remain valid for the loaded module lifetime.
        unsafe {
            module.add_api(
                c"rust_webhook_stats",
                c"prints webhook delivery counters",
                c"rust_webhook_stats",
                stats_api,
            )
        },
    ] {
        if let Err(error) = result {
            return error.0;
        }
    }

    SUCCESS
}

fn post_webhook(request: &WebhookRequest) -> std::io::Result<()> {
    let mut stream = TcpStream::connect((request.url.host.as_str(), request.url.port))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;

    write!(
        stream,
        "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        request.url.path,
        request.url.host,
        request.body.len(),
        request.body
    )?;

    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    if response.starts_with("HTTP/1.1 2") || response.starts_with("HTTP/1.0 2") {
        Ok(())
    } else {
        Err(std::io::Error::other("non-success webhook response"))
    }
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
