use std::{thread, time::Duration};

use fswtch::SUCCESS;
use serde_json::Value;

const REGISTRATION_CHECK_DELAY: Duration = Duration::from_millis(150);

fswtch::module_exports! {
    module = mod_registration_check,
    load = switch_module_load,
}

fswtch::api_callback! {
    fn check_registration_api(cmd, _session, stream) {
        fswtch::log_info("mod_registration_check", "rust_check_registration invoked");
        let Some(request) = cmd.as_deref().and_then(RegistrationRequest::parse) else {
            fswtch::log_info("mod_registration_check", "invalid command syntax");
            let status =
                stream.write("usage: rust_check_registration <user@domain> <https://server/check>\n");
            return fswtch::false_on_success(status);
        };

        let status = stream.write("registration check queued\n");
        if status != SUCCESS {
            return status;
        }

        let worker = thread::Builder::new()
            .name("fswtch-registration-check".to_owned())
            .spawn(move || {
                fswtch::log_info(
                    "mod_registration_check",
                    format!("worker started for {}", request.user),
                );
                let result = check_registration_remotely(&request);
                if let Err(error) = fire_registration_event(&request, &result) {
                    fswtch::log_error(
                        "mod_registration_check",
                        format!("failed to fire registration check event: {error}"),
                    );
                } else {
                    fswtch::log_info("mod_registration_check", "worker fired result event");
                }
            });
        if let Err(error) = worker {
            fswtch::log_error(
                "mod_registration_check",
                format!("failed to start registration check worker: {error}"),
            );
            return fswtch::GENERR;
        }

        SUCCESS
    }
}

fswtch::module_load! {
    fn switch_module_load(module) for c"mod_registration_check" {
        fswtch::log_info("mod_registration_check", "loading module");
        module.api(
            c"rust_check_registration",
            c"asynchronously validates a registration and fires a custom event",
            c"rust_check_registration <user@domain> <https://server/check>",
            check_registration_api,
        )
    }
}

#[derive(Debug, Clone)]
struct RegistrationRequest {
    user: String,
    server_url: String,
}

impl RegistrationRequest {
    fn parse(text: &str) -> Option<Self> {
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
    thread::sleep(REGISTRATION_CHECK_DELAY);

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
    let mut event = fswtch::Event::custom(c"fswtch::registration_check")?;
    event.add_header(c"Registration-User", &request.user)?;
    event.add_header(c"Registration-Server", &request.server_url)?;
    event.add_header(
        c"Registration-Accepted",
        if result.accepted { "true" } else { "false" },
    )?;
    event.add_header(c"Registration-Score", &result.score.to_string())?;
    event.add_header(c"Registration-Reason", &result.reason)?;
    event.add_header(c"Registration-Request-ID", &result.request_id)?;
    event.fire()
}
