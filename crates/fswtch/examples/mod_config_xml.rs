use std::sync::{LazyLock, Mutex};

static CONFIG: LazyLock<Mutex<Config>> = LazyLock::new(|| Mutex::new(Config::default()));

fswtch::module_exports! {
    module = mod_config_xml,
    load = switch_module_load,
}

#[derive(Debug, Clone)]
struct Config {
    enabled: bool,
    greeting: String,
    max_sessions: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            greeting: "hello from XML config".to_owned(),
            max_sessions: 8,
        }
    }
}

fswtch::api_callback! {
    fn show_api(_cmd, _session, stream) {
        fswtch::log_info("mod_config_xml", "rust_config_xml_show invoked");
        let Some(stream) = stream else {
            return fswtch::FALSE;
        };
        let config = CONFIG
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        stream.write(
            &format!(
                "enabled={} greeting={} max_sessions={}\n",
                config.enabled, config.greeting, config.max_sessions
            ),
        )
    }
}

fswtch::api_callback! {
    fn reload_api(_cmd, _session, stream) {
        fswtch::log_info("mod_config_xml", "rust_config_xml_reload invoked");
        let Some(stream) = stream else {
            return fswtch::FALSE;
        };
        match load_config() {
            Ok(config) => {
                *CONFIG
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = config;
                stream.write("config reloaded\n")
            }
            Err(error) => {
                stream.write(&format!("config reload failed: {error}\n"))
            }
        }
    }
}

fswtch::module_load! {
    fn switch_module_load(module) for "mod_config_xml" {
        fswtch::log_info("mod_config_xml", "loading module");
        if let Ok(config) = load_config() {
            *CONFIG
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = config;
        }
        module
            .api(
                "rust_config_xml_show",
                "prints settings loaded from fswtch_examples.conf",
                "rust_config_xml_show",
                show_api,
            )
            .and_then(|module| {
                module.api(
                    "rust_config_xml_reload",
                    "reloads settings from fswtch_examples.conf",
                    "rust_config_xml_reload",
                    reload_api,
                )
            })
    }
}

fn load_config() -> Result<Config, &'static str> {
    fswtch::log_info("mod_config_xml", "loading fswtch_examples.conf");
    let mut config = Config::default();
    let xml =
        fswtch::XmlConfig::open("fswtch_examples.conf").ok_or("fswtch_examples.conf not found")?;

    if let Some(settings) = xml.settings().and_then(|node| node.child("settings")) {
        parse_settings(settings, &mut config);
    }

    Ok(config)
}

fn parse_settings(settings: fswtch::XmlNode, config: &mut Config) {
    let mut param = settings.child("param");
    while let Some(node) = param {
        let Some(name) = node.attr("name") else {
            param = node.next();
            continue;
        };
        let Some(value) = node.attr("value") else {
            param = node.next();
            continue;
        };

        match name.as_str() {
            "enabled" => config.enabled = matches!(value.as_str(), "true" | "yes" | "1"),
            "greeting" => config.greeting = value,
            "max-sessions" => {
                if let Ok(parsed) = value.parse() {
                    config.max_sessions = parsed;
                }
            }
            _ => {}
        }

        param = node.next();
    }
}
