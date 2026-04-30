use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
    time::{Duration, Instant},
};

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
    fn parse(text: &str) -> Option<Self> {
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

fswtch::api_callback! {
    fn allow_api(cmd, _session, stream) {
        fswtch::log_info("mod_rate_limiter", "rust_rate_limit invoked");
        let Some(request) = cmd.as_deref().and_then(LimitRequest::parse) else {
            let status = stream.write("usage: rust_rate_limit <key> [limit] [window-secs]\n");
            return fswtch::false_on_success(status);
        };

        let now = Instant::now();
        let mut limiters = LIMITERS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !limiters.contains_key(&request.key) && limiters.len() >= MAX_BUCKETS {
            fswtch::log_error("mod_rate_limiter", "rate limiter bucket limit reached");
            return stream.write("rate limiter bucket limit reached\n");
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
        fswtch::log_info(
            "mod_rate_limiter",
            format!(
                "key={} allowed={} remaining={}",
                request.key, allowed, bucket.remaining
            ),
        );

        stream.write(
            &format!(
                "key={} allowed={} remaining={}\n",
                request.key, allowed, bucket.remaining
            ),
        )
    }
}

fswtch::api_callback! {
    fn reset_api(_cmd, _session, stream) {
        fswtch::log_info("mod_rate_limiter", "rust_rate_limit_reset invoked");
        LIMITERS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        stream.write("rate limiters reset\n")
    }
}

fswtch::module_load! {
    fn switch_module_load(module) for c"mod_rate_limiter" {
        fswtch::log_info("mod_rate_limiter", "loading module");
        module
            .api(
                c"rust_rate_limit",
                c"checks a token-bucket rate limit",
                c"rust_rate_limit <key> [limit] [window-secs]",
                allow_api,
            )
            .and_then(|module| {
                module.api(
                    c"rust_rate_limit_reset",
                    c"clears all rate limiter buckets",
                    c"rust_rate_limit_reset",
                    reset_api,
                )
            })
    }
}
