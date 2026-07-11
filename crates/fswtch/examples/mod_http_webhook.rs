use std::{
    io::{Read, Write},
    net::TcpStream,
    sync::atomic::{AtomicUsize, Ordering},
    thread,
    time::Duration,
};

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
    fn parse(text: &str) -> Option<Self> {
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

fswtch::api_callback! {
    fn post_api(cmd, _session, stream) {
        fswtch::log_info("mod_http_webhook", "fswtch_webhook_post invoked");
        let Some(stream) = stream else {
            return fswtch::FALSE;
        };
        let Some(request) = cmd.as_deref().and_then(WebhookRequest::parse) else {
            fswtch::log_info("mod_http_webhook", "invalid webhook command");
            let status = stream.write("usage: fswtch_webhook_post <http-url> <json-body>\n");
            return fswtch::false_on_success(status);
        };

        WEBHOOKS_QUEUED.fetch_add(1, Ordering::Relaxed);
        let worker = thread::Builder::new()
            .name("fswtch-http-webhook".to_owned())
            .spawn(move || match post_webhook(&request) {
                Ok(()) => {
                    fswtch::log_info("mod_http_webhook", "webhook delivered");
                    WEBHOOKS_SENT.fetch_add(1, Ordering::Relaxed);
                }
                Err(error) => {
                    WEBHOOKS_FAILED.fetch_add(1, Ordering::Relaxed);
                    fswtch::log_error(
                        "mod_http_webhook",
                        format!("webhook delivery failed: {error}"),
                    );
                }
            });
        if worker.is_err() {
            return fswtch::GENERR;
        }

        stream.write("webhook queued\n")
    }
}

fswtch::api_callback! {
    fn stats_api(_cmd, _session, stream) {
        fswtch::log_info("mod_http_webhook", "fswtch_webhook_stats invoked");
        let Some(stream) = stream else {
            return fswtch::FALSE;
        };
        stream.write(
            &format!(
                "queued={} sent={} failed={}\n",
                WEBHOOKS_QUEUED.load(Ordering::Relaxed),
                WEBHOOKS_SENT.load(Ordering::Relaxed),
                WEBHOOKS_FAILED.load(Ordering::Relaxed)
            ),
        )
    }
}

fswtch::module_load! {
    fn switch_module_load(module) for "mod_http_webhook" {
        fswtch::log_info("mod_http_webhook", "loading module");
        module
            .api(
                "fswtch_webhook_post",
                "queues a plain HTTP webhook POST",
                "fswtch_webhook_post <http-url> <json-body>",
                post_api,
            )
            .and_then(|module| {
                module.api(
                    "fswtch_webhook_stats",
                    "prints webhook delivery counters",
                    "fswtch_webhook_stats",
                    stats_api,
                )
            })
    }
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
