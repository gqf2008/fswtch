use std::{
    ffi::c_char,
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
};

use fswtch::{FALSE, Module, SUCCESS, Status, Stream, sys};

static BUGS_ATTACHED: AtomicUsize = AtomicUsize::new(0);
static BUGS_CLOSED: AtomicUsize = AtomicUsize::new(0);
static FRAMES_SEEN: AtomicUsize = AtomicUsize::new(0);
static AUDIO_BYTES_SEEN: AtomicUsize = AtomicUsize::new(0);

fswtch::module_exports! {
    module = mod_media_bug_meter,
    load = switch_module_load,
}

#[derive(Debug)]
struct MeterState {
    frames: usize,
    audio_bytes: usize,
}

// SAFETY: FreeSWITCH calls this function with pointers matching `switch_application_function_t`.
unsafe extern "C" fn meter_app(session: *mut sys::switch_core_session_t, _data: *const c_char) {
    if session.is_null() {
        return;
    }

    let state = Box::into_raw(Box::new(MeterState {
        frames: 0,
        audio_bytes: 0,
    }));
    let mut bug = ptr::null_mut();
    let flags = sys::switch_media_bug_flag_enum_t_SMBF_READ_STREAM
        | sys::switch_media_bug_flag_enum_t_SMBF_NO_PAUSE;

    // SAFETY: `session` is live for this application invocation; ownership of `state` is given to
    // the bug callback and reclaimed on close when FreeSWITCH accepts the media bug.
    let status = unsafe {
        sys::switch_core_media_bug_add(
            session,
            c"rust_media_bug_meter".as_ptr(),
            c"read-stream".as_ptr(),
            Some(meter_callback),
            state.cast(),
            0,
            flags,
            &mut bug,
        )
    };

    if status == SUCCESS {
        BUGS_ATTACHED.fetch_add(1, Ordering::Relaxed);
    } else {
        // SAFETY: FreeSWITCH did not take ownership when add failed.
        unsafe {
            drop(Box::from_raw(state));
        }
    }
}

// SAFETY: FreeSWITCH calls this function with pointers matching `switch_media_bug_callback_t`.
unsafe extern "C" fn meter_callback(
    bug: *mut sys::switch_media_bug_t,
    user_data: *mut std::ffi::c_void,
    callback_type: sys::switch_abc_type_t,
) -> sys::switch_bool_t {
    if user_data.is_null() {
        return sys::switch_bool_t_SWITCH_TRUE;
    }

    if callback_type == sys::switch_abc_type_t_SWITCH_ABC_TYPE_READ {
        // SAFETY: `user_data` is the `MeterState` pointer passed to `switch_core_media_bug_add`.
        let state = unsafe { &mut *user_data.cast::<MeterState>() };
        // SAFETY: `bug` is live for the callback duration.
        let frame = unsafe { sys::switch_core_media_bug_get_native_read_frame(bug) };
        if !frame.is_null() {
            // SAFETY: `frame` is owned by FreeSWITCH and valid for this callback.
            let bytes = unsafe { (*frame).datalen as usize };
            state.frames += 1;
            state.audio_bytes += bytes;
            FRAMES_SEEN.fetch_add(1, Ordering::Relaxed);
            AUDIO_BYTES_SEEN.fetch_add(bytes, Ordering::Relaxed);
        }
    } else if callback_type == sys::switch_abc_type_t_SWITCH_ABC_TYPE_CLOSE {
        // SAFETY: Reclaims the box allocated in `meter_app`; close is the terminal callback.
        unsafe {
            drop(Box::from_raw(user_data.cast::<MeterState>()));
        }
        BUGS_CLOSED.fetch_add(1, Ordering::Relaxed);
    }

    sys::switch_bool_t_SWITCH_TRUE
}

// SAFETY: FreeSWITCH calls this function with pointers matching `switch_api_function_t`.
unsafe extern "C" fn stats_api(
    _cmd: *const c_char,
    _session: *mut sys::switch_core_session_t,
    stream: *mut sys::switch_stream_handle_t,
) -> Status {
    write_response(
        stream,
        &format!(
            "attached={} closed={} frames={} audio_bytes={}\n",
            BUGS_ATTACHED.load(Ordering::Relaxed),
            BUGS_CLOSED.load(Ordering::Relaxed),
            FRAMES_SEEN.load(Ordering::Relaxed),
            AUDIO_BYTES_SEEN.load(Ordering::Relaxed)
        ),
    )
}

// SAFETY: FreeSWITCH calls this function during module load with loader-owned pointers.
unsafe extern "C" fn switch_module_load(
    module_interface: *mut *mut sys::switch_loadable_module_interface_t,
    pool: *mut sys::switch_memory_pool_t,
) -> Status {
    // SAFETY: The loader passes the module slot and pool, and the module name is static.
    let module = match unsafe { Module::create(module_interface, pool, c"mod_media_bug_meter") } {
        Ok(module) => module,
        Err(error) => return error.0,
    };

    // SAFETY: The module interface is live, and assigned C strings/function pointer are static.
    if unsafe { add_application(module.as_ptr()) }.is_none() {
        return fswtch::GENERR;
    }

    // SAFETY: The callback and C strings remain valid for the loaded module lifetime.
    if let Err(error) = unsafe {
        module.add_api(
            c"rust_media_bug_meter_stats",
            c"prints media bug meter counters",
            c"rust_media_bug_meter_stats",
            stats_api,
        )
    } {
        return error.0;
    }

    SUCCESS
}

unsafe fn add_application(
    module: *mut sys::switch_loadable_module_interface_t,
) -> Option<*mut sys::switch_application_interface_t> {
    // SAFETY: `module` is a live module interface created by FreeSWITCH.
    let raw = unsafe {
        sys::switch_loadable_module_create_interface(
            module,
            sys::switch_module_interface_name_t::SWITCH_APPLICATION_INTERFACE,
        )
    }
    .cast::<sys::switch_application_interface_t>();
    if raw.is_null() {
        return None;
    }

    // SAFETY: `raw` points to a FreeSWITCH application interface allocation.
    unsafe {
        (*raw).interface_name = c"rust_media_bug_meter".as_ptr();
        (*raw).application_function = Some(meter_app);
        (*raw).long_desc =
            c"Attaches a read-stream media bug and counts observed audio frames".as_ptr();
        (*raw).short_desc = c"Rust media bug meter example".as_ptr();
        (*raw).syntax = c"rust_media_bug_meter".as_ptr();
    }

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
