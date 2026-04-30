use std::sync::atomic::{AtomicUsize, Ordering};

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

fswtch::api_callback! {
    fn enrich_api(cmd, _session, stream) {
        fswtch::log_info("mod_cdr_enricher", "rust_cdr_enrich invoked");
        let Some(text) = cmd else {
            fswtch::log_info("mod_cdr_enricher", "missing CDR JSON");
            let status = stream.write("usage: rust_cdr_enrich <json-cdr>\n");
            return fswtch::false_on_success(status);
        };

        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            fswtch::log_info("mod_cdr_enricher", "invalid CDR JSON");
            let status = stream.write("invalid cdr json\n");
            return fswtch::false_on_success(status);
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
        stream.write(&format!("{response}\n"))
    }
}

fswtch::api_callback! {
    fn stats_api(_cmd, _session, stream) {
        fswtch::log_info("mod_cdr_enricher", "rust_cdr_enricher_stats invoked");
        stream.write(
            &format!("cdrs_enriched={}\n", CDRS_ENRICHED.load(Ordering::Relaxed)),
        )
    }
}

fswtch::module_load! {
    fn switch_module_load(module) for "mod_cdr_enricher" {
        fswtch::log_info("mod_cdr_enricher", "loading module");
        module
            .api(
                "rust_cdr_enrich",
                "enriches a CDR JSON document and emits a custom event",
                "rust_cdr_enrich <json-cdr>",
                enrich_api,
            )
            .and_then(|module| {
                module.api(
                    "rust_cdr_enricher_stats",
                    "prints CDR enrichment counters",
                    "rust_cdr_enricher_stats",
                    stats_api,
                )
            })
    }
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
    let mut event = fswtch::Event::custom("fswtch::cdr_enriched")?;
    event.add_header("CDR-UUID", &cdr.uuid)?;
    event.add_header("CDR-Account", &cdr.account)?;
    event.add_header("CDR-Account-Tier", cdr.tier)?;
    event.add_header("CDR-Risk-Score", &cdr.risk.to_string())?;
    event.add_header("CDR-Billable-Seconds", &cdr.billable_seconds.to_string())?;
    event.fire()
}
