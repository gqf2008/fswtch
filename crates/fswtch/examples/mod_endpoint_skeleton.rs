use std::{ffi::c_char, ptr};

use fswtch::{Module, SUCCESS, Status, sys};

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

// SAFETY: FreeSWITCH calls this function with pointers matching `switch_api_function_t`.
unsafe extern "C" fn info_api(
    _cmd: *const c_char,
    _session: *mut sys::switch_core_session_t,
    stream: *mut sys::switch_stream_handle_t,
) -> Status {
    fswtch::log_info(
        "mod_endpoint_skeleton",
        "rust_endpoint_skeleton_info invoked",
    );
    fswtch::write_stream_response(
        stream,
        "endpoint rust_endpoint_skeleton registered with placeholder I/O routines\n",
    )
}

// SAFETY: FreeSWITCH calls this function during module load with loader-owned pointers.
unsafe extern "C" fn switch_module_load(
    module_interface: *mut *mut sys::switch_loadable_module_interface_t,
    pool: *mut sys::switch_memory_pool_t,
) -> Status {
    fswtch::log_info("mod_endpoint_skeleton", "loading module");
    let module = match Module::create(module_interface, pool, c"mod_endpoint_skeleton") {
        Ok(module) => module,
        Err(error) => return error.0,
    };

    if let Err(error) = module.add_endpoint(c"rust_endpoint_skeleton", &raw mut IO_ROUTINES) {
        return error.0;
    }
    fswtch::log_info("mod_endpoint_skeleton", "endpoint interface registered");

    if let Err(error) = module.add_api(
        c"rust_endpoint_skeleton_info",
        c"describes the Rust endpoint skeleton",
        c"rust_endpoint_skeleton_info",
        info_api,
    ) {
        return error.0;
    }

    SUCCESS
}
