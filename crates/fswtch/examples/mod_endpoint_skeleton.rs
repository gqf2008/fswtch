/// Minimal endpoint skeleton: registers an endpoint interface whose `kill_channel` I/O routine
/// is wired through the safe `EndpointIoRoutines` trait (`EndpointIoBuilder::build::<T>()`), with
/// every other routine left at its safe default. No `*-sys` types appear in module code.
struct SkeletonEndpoint;

impl fswtch::EndpointIoRoutines for SkeletonEndpoint {
    const NAME: &'static str = "fswtch_endpoint_skeleton";

    fn kill_channel(_session: &fswtch::Session, _sig: i32) -> fswtch::Status {
        fswtch::log_info("mod_endpoint_skeleton", "kill_channel invoked");
        fswtch::SUCCESS
    }

    fn state_change(_session: &fswtch::Session) -> fswtch::Status {
        fswtch::log_info("mod_endpoint_skeleton", "state_change invoked");
        fswtch::SUCCESS
    }
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
        stream.write("endpoint fswtch_endpoint_skeleton registered with kill_channel + state_change routines\n")
    }
}

fswtch::module_load! {
    fn switch_module_load(module) for "mod_endpoint_skeleton" {
        fswtch::log_info("mod_endpoint_skeleton", "loading module");

        // Build the I/O routines table through the safe trait, then register the endpoint
        // interface. The table is allocated for the module's lifetime (intentionally leaked by
        // `EndpointIoBuilder::build`), so the pointer remains valid for the interface's lifetime.
        fswtch::EndpointIoBuilder::build::<SkeletonEndpoint>()
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
