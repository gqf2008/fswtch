#![allow(clippy::not_unsafe_ptr_arg_deref)]
// Many `pub(crate)` FFI-bridge methods (raw `as_ptr`/`from_raw`/`*Fn` aliases, and the
// sys-typed wrappers like `queue_dtmf`/`dequeue_event`/`set_cap_value`) are deliberately kept as
// the complete internal escape surface for FreeSWITCH calls, even where this crate's own examples
// and tests do not exercise every one. They are `pub(crate)` precisely so no `*-sys` type leaks
// into the public API; downstream code uses the safe newtype-taking methods instead. Suppress
// dead-code so the binding surface stays complete without per-item `#[allow]` noise.
#![allow(dead_code)]

mod buffer;
mod caller;
mod channel;
mod codec;
mod command;
mod console;
mod core;
mod core_db;
mod endpoint;
mod estimators;
mod event;
mod exports;
mod file;
mod ivr;
mod jitterbuffer;
mod limit;
mod logging;
mod media;
mod module;
mod nat;
mod network_list;
mod packetizer;
mod plc;
mod pool;
mod regex;
mod resample;
mod rtp;
mod scheduler;
mod session;
mod speex;
mod status;
mod stream;
mod timer;
mod utils;
mod vad;
mod video;
mod xml;

pub mod dsp;

// The raw `fswtch_sys` crate is an internal implementation detail. It is `pub(crate)` so the
// safe wrappers can name C types internally, but it is deliberately NOT part of `fswtch`'s
// public API: no `*-sys` type appears in any documented signature. The `#[macro_export]`
// macros below never reference `$crate::sys`; the only FFI struct the macros materialize in a
// downstream crate (`module_exports!`'s module-interface table) is constructed through the
// `#[doc(hidden)]` `__ModuleFunctionTable` wrapper, so even that path is sys-free at the call site.
pub(crate) use fswtch_sys as sys;

pub use buffer::*;
pub use caller::*;
pub use channel::*;
pub use codec::*;
pub use command::{
    StaticCStr, borrowed_cstr_to_str, borrowed_cstr_to_string, command_text, cstring, free_cstr,
    strdup_to_string,
};
pub use console::{
    CompletionFunc, CompletionMatches, complete, execute, execute_api, expand_alias,
};
pub use core::{
    get_domain, get_hostname, get_switchname, get_uuid, get_variable, hupall, hupall_endpoint,
    hupall_matching_var, hupall_matching_vars, session_count, sessions_per_second, set_variable,
    uptime,
};
pub use core_db::{CacheDbType, CoreDb, Stmt, StmtRows, TableRows, cache_db_type};
pub use endpoint::{
    Dtmf, DtmfSource, EndpointInterfaceRef, EndpointIoBuilder, EndpointIoRoutines, Frame, FrameMut,
    IoFlags, IoRoutines, MessageType, OutgoingResult, SessionMessage, StateHandlerTable,
    request_session,
};
pub use estimators::{CusumDetector, KalmanEstimator, is_slow_link};
pub use event::{
    Event, EventBinder, EventRef, EventType, EventXml, HeaderIter, Priority,
    channel_permission_clear, channel_permission_modify, channel_permission_verify, event_running,
};
pub use file::*;
pub use ivr::*;
pub use jitterbuffer::{JbFlag, JbFrames, JbKind, JitterBuffer, JitterBufferConfig};
pub use limit::{Usage, backend, fire_event, incr, interval_reset, release, reset, status, usage};
pub use logging::*;
pub use media::*;
pub use module::{
    ApiInterface, ApplicationInfo, ApplicationInterface, AsrInterface, ChatApplicationInterface,
    ChatInterface, DatabaseInterface, DialplanInterface, DirectoryInterface, EndpointInterface,
    FileInterface, JsonApiInterface, LimitInterface, ManagementInterface, Module, ModuleBuilder,
    SayInterface, SpeechInterface, TimerInterface,
};
pub use nat::{
    add_mapping as nat_add_mapping, del_mapping as nat_del_mapping, init as nat_init,
    is_initialized as nat_is_initialized, late_init as nat_late_init, reinit as nat_reinit,
    republish as nat_republish, set_mapping as nat_set_mapping, shutdown as nat_shutdown,
    type_str as nat_type_str,
};
// `#[doc(hidden)]` FFI glue used only by the `module_exports!` macro to build a module-interface
// table without naming a `*-sys` type at the call site. Not part of the public API.
#[doc(hidden)]
pub use module::__ModuleFunctionTable;
pub use network_list::{AclVerdict, NetworkList};
pub use packetizer::{BitstreamType, Packetizer};
pub use plc::Plc;
pub use pool::*;
pub use regex::{CaptureCallback, Regex, RegexMatch, is_match, is_match_partial};
pub use resample::{
    Agc, AgcConfig, DEFAULT_QUALITY, Resample, calc_buffer_size, change_sln_volume,
    change_sln_volume_granular, char_to_float, float_to_char, float_to_short, generate_sln_silence,
    merge_sln, mux_channels, short_to_float, swap_linear, unmerge_sln,
};
pub use rtp::*;
pub use scheduler::{
    Task, TaskConfig, TaskFlags, TaskHandle, TaskHandler, cancel_group, spawn, start, stop,
};
pub use session::*;
pub use speex::*;
pub use status::{
    CAUSE_REQUESTED_CHAN_UNAVAIL, CAUSE_SUCCESS, CallDirection, Cause, ChannelState, FALSE, GENERR,
    HupType, OriginateFlag, Result, SUCCESS, Status, SwitchError, false_on_success,
    status_to_result,
};
pub use stream::*;
pub use timer::Timer;
pub use utils::{escape_string, find_end_paren, format_number, url_encode};
pub use vad::{SpeechSegment, Vad, VadEngine, VadState, snap_segments};
pub use video::{
    CachedImage, Chromakey, Color, Image, ImageFit, ImageFormat, ImagePosition, Shade,
};
pub use xml::*;

#[macro_export]
macro_rules! api_callback {
    (fn $name:ident($cmd:ident, $session:ident, $stream:ident) $body:block) => {
        // FFI boundary: returns `$crate::Status` directly. `Status` is `#[repr(transparent)]`
        // over `switch_status_t`, so `extern "C" fn() -> Status` has the identical ABI to
        // `-> switch_status_t` that FreeSWITCH expects — no `.raw()` conversion needed.
        // Pointer parameters are `*mut std::ffi::c_void` (pointee-erased; ABI-identical to the
        // real FreeSWITCH pointer types), so the macro never names a `sys` type. The user's
        // `$body` runs in an inner closure returning `fswtch::Status`; early `return` inside
        // the body returns from the closure.
        // The whole body is wrapped in `catch_unwind` so a panic cannot unwind across the
        // `unsafe extern "C"` boundary into FreeSWITCH (UB / aborts the whole FS process). On
        // panic we log via `fswtch::log_error` and return `Status::GENERR`. This relies on the
        // downstream crate being built with `panic = "unwind"` (the default; the mod_* crates
        // set it explicitly).
        unsafe extern "C" fn $name(
            cmd_raw: *const ::std::ffi::c_char,
            session_raw: *mut ::std::ffi::c_void,
            stream_raw: *mut ::std::ffi::c_void,
        ) -> $crate::Status {
            let result = ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| {
                let body = |$cmd: Option<String>,
                            $session: Option<$crate::Session>,
                            $stream: Option<$crate::ApiStream>|
                 -> $crate::Status { $body };
                let $cmd = unsafe { $crate::command_text(cmd_raw) };
                let $session = unsafe { $crate::Session::from_raw(session_raw) };
                let $stream = unsafe { $crate::ApiStream::from_raw(stream_raw) };
                body($cmd, $session, $stream)
            }));
            match result {
                ::std::result::Result::Ok(status) => status,
                ::std::result::Result::Err(panic) => {
                    $crate::log_error(
                        "fswtch",
                        ::std::format!("panic in api callback {}: {:?}", stringify!($name), panic),
                    );
                    $crate::Status::GENERR
                }
            }
        }
    };
}

#[macro_export]
macro_rules! app_callback {
    (fn $name:ident($session:ident, $data:ident) $body:block) => {
        // See `api_callback!` — the body is wrapped in `catch_unwind` so a panic cannot
        // unwind across the `unsafe extern "C"` boundary into FreeSWITCH. app callbacks
        // return unit, so on panic we just log and return. The session pointer is passed as
        // `*mut c_void` (pointee-erased); no `sys` type is named by the macro.
        unsafe extern "C" fn $name(
            $session: *mut ::std::ffi::c_void,
            $data: *const ::std::ffi::c_char,
        ) {
            let result = ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| {
                let $session = unsafe { $crate::Session::from_raw($session) };
                let $data = unsafe { $crate::command_text($data) };
                $body
            }));
            if let ::std::result::Result::Err(panic) = result {
                $crate::log_error(
                    "fswtch",
                    ::std::format!("panic in app callback {}: {:?}", stringify!($name), panic),
                );
            }
        }
    };
}

#[macro_export]
macro_rules! chat_callback {
    (fn $name:ident($event:ident, $data:ident) $body:block) => {
        // See `api_callback!` — returning `$crate::Status` (repr-transparent over
        // `switch_status_t`, ABI-identical), wrapped in `catch_unwind` so a panic cannot
        // unwind across the `unsafe extern "C"` boundary into FreeSWITCH. Pointer params are
        // pointee-erased `*mut c_void`; the macro names no `sys` type.
        unsafe extern "C" fn $name(
            event_raw: *mut ::std::ffi::c_void,
            data_raw: *const ::std::ffi::c_char,
        ) -> $crate::Status {
            let result = ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| {
                let body =
                    |$event: $crate::EventRef, $data: Option<String>| -> $crate::Status { $body };
                let $event = unsafe { $crate::EventRef::from_raw(event_raw) };
                let $data = unsafe { $crate::command_text(data_raw) };
                body($event, $data)
            }));
            match result {
                ::std::result::Result::Ok(status) => status,
                ::std::result::Result::Err(panic) => {
                    $crate::log_error(
                        "fswtch",
                        ::std::format!("panic in chat callback {}: {:?}", stringify!($name), panic),
                    );
                    $crate::Status::GENERR
                }
            }
        }
    };
}

/// Declares an `unsafe extern "C" fn` matching FreeSWITCH's `switch_event_callback_t`, wrapping the
/// raw event pointer in an [`EventRef`](crate::EventRef) for a safe body. Use the resulting function
/// pointer with [`EventBinder::bind`](crate::EventBinder::bind).
#[macro_export]
macro_rules! event_callback {
    (fn $name:ident($event:ident) $body:block) => {
        // See `api_callback!` — the body is wrapped in `catch_unwind` so a panic cannot
        // unwind across the `unsafe extern "C"` boundary into FreeSWITCH. Event callbacks
        // return unit, so on panic we just log and return. The event pointer is passed as
        // `*mut c_void` (pointee-erased); no `sys` type is named by the macro.
        unsafe extern "C" fn $name($event: *mut ::std::ffi::c_void) {
            let result = ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| {
                let $event = unsafe { $crate::EventRef::from_raw($event) };
                $body
            }));
            if let ::std::result::Result::Err(panic) = result {
                $crate::log_error(
                    "fswtch",
                    ::std::format!("panic in event callback {}: {:?}", stringify!($name), panic),
                );
            }
        }
    };
}

#[macro_export]
macro_rules! module_load {
    (fn $name:ident($module:ident) for $module_name:literal $body:block) => {
        // Returns `$crate::Status` directly at the FFI boundary (repr-transparent over
        // `switch_status_t`, ABI-identical); the user's `$body` produces `fswtch::Result`,
        // mapped to a `Status` here. Pointer params are pointee-erased `*mut c_void`; the
        // macro names no `sys` type.
        unsafe extern "C" fn $name(
            module_interface: *mut *mut ::std::ffi::c_void,
            pool: *mut ::std::ffi::c_void,
        ) -> $crate::Status {
            // See `api_callback!` — the whole body is wrapped in `catch_unwind` so a panic cannot
            // unwind across the `unsafe extern "C"` boundary into FreeSWITCH. Module load returns
            // a `Status`, so on panic we log and return `Status::GENERR`.
            let result = ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| {
                let $module =
                    match unsafe { $crate::ModuleBuilder::new(module_interface, pool, $module_name) }
                    {
                        Ok(module) => module,
                        Err(error) => return error.status(),
                    };
                let result: $crate::Result<$crate::ModuleBuilder> = $body;
                match result {
                    Ok(_) => $crate::Status::SUCCESS,
                    Err(error) => error.status(),
                }
            }));
            match result {
                ::std::result::Result::Ok(status) => status,
                ::std::result::Result::Err(panic) => {
                    $crate::log_error(
                        "fswtch",
                        ::std::format!(
                            "panic in module load {}: {:?}",
                            stringify!($name),
                            panic
                        ),
                    );
                    $crate::Status::GENERR
                }
            }
        }
    };
}
