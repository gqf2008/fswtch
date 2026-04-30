use std::{
    ffi::c_char,
    sync::atomic::{AtomicUsize, Ordering},
};

use fswtch::{
    FALSE, MediaBugAction, MediaBugConfig, MediaBugContext, MediaBugFlags, MediaBugHandler,
    MediaFrame, Module, SUCCESS, Status, Stream, sys,
};

static BUGS_ATTACHED: AtomicUsize = AtomicUsize::new(0);
static BUGS_CLOSED: AtomicUsize = AtomicUsize::new(0);
static READ_FRAMES_SEEN: AtomicUsize = AtomicUsize::new(0);
static WRITE_FRAMES_SEEN: AtomicUsize = AtomicUsize::new(0);
static READ_AUDIO_BYTES_SEEN: AtomicUsize = AtomicUsize::new(0);
static WRITE_AUDIO_BYTES_SEEN: AtomicUsize = AtomicUsize::new(0);

fswtch::module_exports! {
    module = mod_media_bug_meter,
    load = switch_module_load,
}

#[derive(Debug)]
struct MeterState {
    read_frames: usize,
    write_frames: usize,
    read_audio_bytes: usize,
    write_audio_bytes: usize,
}

impl MediaBugHandler for MeterState {
    fn on_read(&mut self, _ctx: &mut MediaBugContext<'_>, frame: MediaFrame<'_>) -> MediaBugAction {
        let bytes = frame.data_len();
        self.read_frames += 1;
        self.read_audio_bytes += bytes;
        READ_FRAMES_SEEN.fetch_add(1, Ordering::Relaxed);
        READ_AUDIO_BYTES_SEEN.fetch_add(bytes, Ordering::Relaxed);
        MediaBugAction::Continue
    }

    fn on_write(
        &mut self,
        _ctx: &mut MediaBugContext<'_>,
        frame: MediaFrame<'_>,
    ) -> MediaBugAction {
        let bytes = frame.data_len();
        self.write_frames += 1;
        self.write_audio_bytes += bytes;
        WRITE_FRAMES_SEEN.fetch_add(1, Ordering::Relaxed);
        WRITE_AUDIO_BYTES_SEEN.fetch_add(bytes, Ordering::Relaxed);
        MediaBugAction::Continue
    }

    fn on_close(&mut self, _ctx: &mut MediaBugContext<'_>) {
        fswtch::log_info("mod_media_bug_meter", "media bug closing");
        BUGS_CLOSED.fetch_add(1, Ordering::Relaxed);
    }
}

// SAFETY: FreeSWITCH calls this function with pointers matching `switch_application_function_t`.
unsafe extern "C" fn meter_app(session: *mut sys::switch_core_session_t, _data: *const c_char) {
    fswtch::log_info("mod_media_bug_meter", "dialplan application invoked");
    if session.is_null() {
        fswtch::log_info("mod_media_bug_meter", "missing session");
        return;
    }

    let config = MediaBugConfig::new(
        c"rust_media_bug_meter",
        c"read-write-stream",
        MediaBugFlags::READ_STREAM | MediaBugFlags::WRITE_STREAM | MediaBugFlags::NO_PAUSE,
    );
    let handler = MeterState {
        read_frames: 0,
        write_frames: 0,
        read_audio_bytes: 0,
        write_audio_bytes: 0,
    };

    // SAFETY: FreeSWITCH provides a live session for this application invocation.
    match unsafe { fswtch::attach_media_bug(session, config, handler) } {
        Ok(_) => {
            BUGS_ATTACHED.fetch_add(1, Ordering::Relaxed);
            fswtch::log_info("mod_media_bug_meter", "media bug attached");
        }
        Err(error) => fswtch::log_error(
            "mod_media_bug_meter",
            format!("failed to attach media bug: {error}"),
        ),
    }
}

// SAFETY: FreeSWITCH calls this function with pointers matching `switch_api_function_t`.
unsafe extern "C" fn stats_api(
    _cmd: *const c_char,
    _session: *mut sys::switch_core_session_t,
    stream: *mut sys::switch_stream_handle_t,
) -> Status {
    fswtch::log_info("mod_media_bug_meter", "rust_media_bug_meter_stats invoked");
    write_response(
        stream,
        &format!(
            "attached={} closed={} read_frames={} write_frames={} read_audio_bytes={} write_audio_bytes={}\n",
            BUGS_ATTACHED.load(Ordering::Relaxed),
            BUGS_CLOSED.load(Ordering::Relaxed),
            READ_FRAMES_SEEN.load(Ordering::Relaxed),
            WRITE_FRAMES_SEEN.load(Ordering::Relaxed),
            READ_AUDIO_BYTES_SEEN.load(Ordering::Relaxed),
            WRITE_AUDIO_BYTES_SEEN.load(Ordering::Relaxed)
        ),
    )
}

// SAFETY: FreeSWITCH calls this function during module load with loader-owned pointers.
unsafe extern "C" fn switch_module_load(
    module_interface: *mut *mut sys::switch_loadable_module_interface_t,
    pool: *mut sys::switch_memory_pool_t,
) -> Status {
    fswtch::log_info("mod_media_bug_meter", "loading module");
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
            c"Attaches a read/write-stream media bug and counts observed audio frames".as_ptr();
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
