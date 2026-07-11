use fswtch::sys;

/// Trampoline for the `kill_channel` I/O routine: logs the hangup and returns success.
unsafe extern "C" fn kill_channel(
    _session: *mut sys::switch_core_session_t,
    _sig: std::os::raw::c_int,
) -> sys::switch_status_t {
    fswtch::log_info("mod_endpoint_skeleton", "kill_channel invoked");
    fswtch::SUCCESS.raw()
}

/// Trampoline for the `state_change` I/O routine: logs the transition and returns success.
unsafe extern "C" fn state_change(
    _session: *mut sys::switch_core_session_t,
) -> sys::switch_status_t {
    fswtch::log_info("mod_endpoint_skeleton", "state_change invoked");
    fswtch::SUCCESS.raw()
}

fswtch::module_exports! {
    module = mod_endpoint_skeleton,
    load = switch_module_load,
}

fswtch::api_callback! {
    fn info_api(_cmd, _session, stream) {
        fswtch::log_info(
            "mod_endpoint_skeleton",
            "fswtch_endpoint_skeleton_info invoked",
        );
        let Some(stream) = stream else {
            return fswtch::FALSE;
        };
        stream.write(
            "endpoint fswtch_endpoint_skeleton registered with kill_channel + state_change routines\n",
        )
    }
}

fswtch::module_load! {
    fn switch_module_load(module) for "mod_endpoint_skeleton" {
        fswtch::log_info("mod_endpoint_skeleton", "loading module");

        // Build the I/O routines table safely with IoRoutinesBuilder, then register the endpoint
        // interface. The table is allocated for the module's lifetime (intentionally leaked by
        // build()), so the pointer remains valid for the interface's lifetime.
        fswtch::IoRoutinesBuilder::new()
            .kill_channel(Some(kill_channel))
            .state_change(Some(state_change))
            .build()
            .and_then(|io| {
                module
                    .endpoint(
                        "fswtch_endpoint_skeleton",
                        io,
                        fswtch::StateHandlerTable::new_null(),
                    )
                    .inspect(|_| {
                        fswtch::log_info(
                            "mod_endpoint_skeleton",
                            "endpoint interface registered",
                        )
                    })
                    .and_then(|module| {
                        module.api(
                            "fswtch_endpoint_skeleton_info",
                            "describes the Rust endpoint skeleton",
                            "fswtch_endpoint_skeleton_info",
                            info_api,
                        )
                    })
            })
    }
}
