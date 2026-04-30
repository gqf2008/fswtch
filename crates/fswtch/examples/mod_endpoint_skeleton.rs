use std::{ffi::c_char, ptr};

use fswtch::{FALSE, Module, SUCCESS, Status, Stream, sys};

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
    fswtch::log_example(
        "mod_endpoint_skeleton",
        "rust_endpoint_skeleton_info invoked",
    );
    write_response(
        stream,
        "endpoint rust_endpoint_skeleton registered with placeholder I/O routines\n",
    )
}

// SAFETY: FreeSWITCH calls this function during module load with loader-owned pointers.
unsafe extern "C" fn switch_module_load(
    module_interface: *mut *mut sys::switch_loadable_module_interface_t,
    pool: *mut sys::switch_memory_pool_t,
) -> Status {
    fswtch::log_example("mod_endpoint_skeleton", "loading module");
    // SAFETY: The loader passes the module slot and pool, and the module name is static.
    let module = match unsafe { Module::create(module_interface, pool, c"mod_endpoint_skeleton") } {
        Ok(module) => module,
        Err(error) => return error.0,
    };

    // SAFETY: The module interface is live, and assigned C strings/static pointers are valid.
    if unsafe { add_endpoint(module.as_ptr()) }.is_none() {
        return fswtch::GENERR;
    }

    // SAFETY: The callback and C strings remain valid for the loaded module lifetime.
    if let Err(error) = unsafe {
        module.add_api(
            c"rust_endpoint_skeleton_info",
            c"describes the Rust endpoint skeleton",
            c"rust_endpoint_skeleton_info",
            info_api,
        )
    } {
        return error.0;
    }

    SUCCESS
}

unsafe fn add_endpoint(
    module: *mut sys::switch_loadable_module_interface_t,
) -> Option<*mut sys::switch_endpoint_interface_t> {
    // SAFETY: `module` is a live module interface created by FreeSWITCH.
    let raw = unsafe {
        sys::switch_loadable_module_create_interface(
            module,
            sys::switch_module_interface_name_t::SWITCH_ENDPOINT_INTERFACE,
        )
    }
    .cast::<sys::switch_endpoint_interface_t>();
    if raw.is_null() {
        return None;
    }

    // SAFETY: `raw` points to a FreeSWITCH endpoint interface allocation. The I/O routine table is
    // static and intentionally empty because this is a registration skeleton, not a call driver.
    unsafe {
        (*raw).interface_name = c"rust_endpoint_skeleton".as_ptr();
        (*raw).io_routines = &raw mut IO_ROUTINES;
    }
    fswtch::log_example("mod_endpoint_skeleton", "endpoint interface registered");

    Some(raw)
}

fn write_response(stream: *mut sys::switch_stream_handle_t, text: &str) -> Status {
    // SAFETY: FreeSWITCH provides a valid stream pointer for the duration of the API callback.
    let Some(mut stream) = (unsafe { Stream::from_raw(stream) }) else {
        return FALSE;
    };

    match stream.write_str(text) {
        Ok(()) => SUCCESS,
        Err(error) => error.0,
    }
}
