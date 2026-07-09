use std::sync::atomic::{AtomicUsize, Ordering};

use fswtch::SUCCESS;

static MESSAGES_BRIDGED: AtomicUsize = AtomicUsize::new(0);

fswtch::module_exports! {
    module = mod_chatbot_bridge,
    load = switch_module_load,
}

const CHATBOT_APP: fswtch::ApplicationInfo = fswtch::ApplicationInfo::new(
    "rust_chatbot_bridge",
    "Transforms inbound chat messages into custom chatbot events",
    "Rust chatbot bridge example",
    "rust_chatbot_bridge <message>",
);

fswtch::chat_callback! {
    fn chatbot_app(event, data) {
        fswtch::log_info("mod_chatbot_bridge", "chat application invoked");
        let text = data.unwrap_or_else(|| "empty chat payload".to_owned());
        let from = event.header("from").unwrap_or_else(|| "unknown".to_owned());
        let to = event.header("to").unwrap_or_else(|| "unknown".to_owned());

        let mut out = match fswtch::Event::custom("fswtch::chatbot_bridge") {
            Ok(event) => event,
            Err(error) => return error.0,
        };

        for result in [
            out.add_header("Chatbot-From", &from),
            out.add_header("Chatbot-To", &to),
            out.add_header("Chatbot-Text", &text),
            out.add_header("Chatbot-Provider", "example-llm"),
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
        let Some(stream) = stream else {
            return fswtch::FALSE;
        };
        stream.write(
            &format!(
                "chatbot_bridge_registered=true messages_bridged={}\n",
                MESSAGES_BRIDGED.load(Ordering::Relaxed)
            ),
        )
    }
}

fswtch::module_load! {
    fn switch_module_load(module) for "mod_chatbot_bridge" {
        fswtch::log_info("mod_chatbot_bridge", "loading module");
        module
            .chat_application(CHATBOT_APP, chatbot_app)
            .and_then(|module| {
                module.api(
                    "rust_chatbot_bridge_stats",
                    "prints chatbot bridge counters",
                    "rust_chatbot_bridge_stats",
                    stats_api,
                )
            })
    }
}
