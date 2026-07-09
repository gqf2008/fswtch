//! Showcase: Channel + CallerProfile + Session.
//!
//! Reads and writes channel variables and walks the caller-profile fields of the
//! session backing an API call. The `rust_channel_show` command is invoked with a
//! session bound, e.g. from the dialplan as `api rust_channel_show`, or from fs_cli
//! against a bridged call.
//!
//! Build a call so the module has a session to inspect:
//!   load mod_channel_vars;
//!   then, with an active channel, run `api rust_channel_show`.
//!
//! When invoked without a session (e.g. straight from `fs_cli rust_channel_show`
//! with no active call), it prints a usage message. With a session it prints the
//! channel name/uuid, the `caller_id_number` variable and `username` caller-profile
//! field, then sets and reads back a made-up variable `rust_channel_var_example`.

fswtch::module_exports! {
    module = mod_channel_vars,
    load = switch_module_load,
}

fswtch::api_callback! {
    fn show_api(_cmd, session, stream) {
        fswtch::log_info("mod_channel_vars", "rust_channel_show invoked");
        let Some(stream) = stream else {
            return fswtch::FALSE;
        };

        let Some(session) = session else {
            return stream.write(
                "usage: rust_channel_show — invoke from a session (api rust_channel_show) so the channel is available\n",
            );
        };

        // `Session` and `Channel` are both `Copy`, so we can pull the channel once
        // and read several fields from it without consuming anything.
        let Some(channel) = session.channel() else {
            return stream.write("error: session has no channel\n");
        };

        let mut out = String::new();

        let name = channel.name().unwrap_or_else(|| "<unset>".to_owned());
        let uuid = channel.uuid().unwrap_or_else(|| "<unset>".to_owned());
        out.push_str(&format!("channel_name={name}\nchannel_uuid={uuid}\n"));

        // Channel variable read. Returns Ok(None) when unset.
        let caller_id = channel
            .variable("caller_id_number")
            .ok()
            .flatten()
            .unwrap_or_else(|| "<unset>".to_owned());
        out.push_str(&format!("caller_id_number={caller_id}\n"));

        // Caller-profile field access (ANI/DNIS/username/context/...).
        let username = channel
            .caller_profile()
            .and_then(|profile| profile.field("username").ok().flatten())
            .unwrap_or_else(|| "<unset>".to_owned());
        out.push_str(&format!("caller_profile.username={username}\n"));

        // Set a made-up variable on the channel and read it straight back.
        match channel.set_variable("rust_channel_var_example", "set-by-mod_channel_vars") {
            Ok(()) => {
                let readback = channel
                    .variable("rust_channel_var_example")
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| "<readback-unset>".to_owned());
                out.push_str(&format!("rust_channel_var_example={readback}\n"));
            }
            Err(error) => {
                out.push_str(&format!("set_variable failed: {error}\n"));
            }
        }

        stream.write(&out)
    }
}

fswtch::module_load! {
    fn switch_module_load(module) for "mod_channel_vars" {
        fswtch::log_info("mod_channel_vars", "loading module");
        module.api(
            "rust_channel_show",
            "shows channel variables and caller-profile fields for the active session",
            "rust_channel_show",
            show_api,
        )
    }
}
