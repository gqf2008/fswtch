use std::ptr;

use fswtch::sys;

static mut IO_ROUTINES: sys::switch_io_routines = sys::switch_io_routines {
    outgoing_channel: None,
    read_frame: None,
    write_frame: None,
    kill_channel: None,
    send_dtmf: None,
    receive_message: None,
    receive_event: None,
    state_change: None,
    read_video_frame: None,
    write_video_frame: None,
    read_text_frame: None,
    write_text_frame: None,
    state_run: None,
    get_jb: None,
    padding: [ptr::null_mut(); 10],
};

fswtch::module_exports! {
    module = mod_endpoint_skeleton,
    load = switch_module_load,
}

fswtch::api_callback! {
    fn info_api(_cmd, _session, stream) {
        fswtch::log_info(
            "mod_endpoint_skeleton",
            "rust_endpoint_skeleton_info invoked",
        );
        stream.write(
            "endpoint rust_endpoint_skeleton registered with placeholder I/O routines\n",
        )
    }
}

fswtch::module_load! {
    fn switch_module_load(module) for c"mod_endpoint_skeleton" {
        fswtch::log_info("mod_endpoint_skeleton", "loading module");
        module
            .endpoint(c"rust_endpoint_skeleton", &raw mut IO_ROUTINES)
            .inspect(|_| fswtch::log_info("mod_endpoint_skeleton", "endpoint interface registered"))
            .and_then(|module| {
                module.api(
                    c"rust_endpoint_skeleton_info",
                    c"describes the Rust endpoint skeleton",
                    c"rust_endpoint_skeleton_info",
                    info_api,
                )
            })
    }
}
