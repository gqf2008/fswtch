use std::sync::atomic::{AtomicUsize, Ordering};

use fswtch::{ModuleBuilder, SUCCESS, Status, sys};

static MESSAGES_BRIDGED: AtomicUsize = AtomicUsize::new(0);

fswtch::module_exports! {
    module = mod_chatbot_bridge,
    load = switch_module_load,
}

fswtch::chat_callback! {
    fn chatbot_app(event, data) {
        fswtch::log_info("mod_chatbot_bridge", "chat application invoked");
        let text = data.unwrap_or_else(|| "empty chat payload".to_owned());
        let from = event.header(c"from").unwrap_or_else(|| "unknown".to_owned());
        let to = event.header(c"to").unwrap_or_else(|| "unknown".to_owned());

        let mut out = match fswtch::Event::custom(c"fswtch::chatbot_bridge") {
            Ok(event) => event,
            Err(error) => return error.0,
        };

        for result in [
            out.add_header(c"Chatbot-From", &from),
            out.add_header(c"Chatbot-To", &to),
            out.add_header(c"Chatbot-Text", &text),
            out.add_header(c"Chatbot-Provider", "example-llm"),
        ] {
            if let Err(error) = result {
                return error.0;
            }
        }

        if let Err(error) = out.fire() {
            return error.0;
        }
        MESSAGES_BRIDGED.fetch_add(1, Ordering::Relaxed);
        fswtch::log_info(
            "mod_chatbot_bridge",
            format!("bridged chat message from={from} to={to}"),
        );

        SUCCESS
    }
}

fswtch::api_callback! {
    fn stats_api(_cmd, _session, stream) {
        fswtch::log_info("mod_chatbot_bridge", "rust_chatbot_bridge_stats invoked");
        stream.write(
            &format!(
                "chatbot_bridge_registered=true messages_bridged={}\n",
                MESSAGES_BRIDGED.load(Ordering::Relaxed)
            ),
        )
    }
}

// SAFETY: FreeSWITCH calls this function during module load with loader-owned pointers.
unsafe extern "C" fn switch_module_load(
    module_interface: *mut *mut sys::switch_loadable_module_interface_t,
    pool: *mut sys::switch_memory_pool_t,
) -> Status {
    fswtch::log_info("mod_chatbot_bridge", "loading module");
    match ModuleBuilder::new(module_interface, pool, c"mod_chatbot_bridge")
        .and_then(|module| {
            module.chat_application(
                c"rust_chatbot_bridge",
                c"Transforms inbound chat messages into custom chatbot events",
                c"Rust chatbot bridge example",
                c"rust_chatbot_bridge <message>",
                chatbot_app,
            )
        })
        .and_then(|module| {
            module.api(
                c"rust_chatbot_bridge_stats",
                c"prints chatbot bridge counters",
                c"rust_chatbot_bridge_stats",
                stats_api,
            )
        }) {
        Ok(_) => SUCCESS,
        Err(error) => error.0,
    }
}
