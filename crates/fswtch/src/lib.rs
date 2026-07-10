#![allow(clippy::not_unsafe_ptr_arg_deref)]

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

pub use fswtch_sys as sys;

pub use buffer::*;
pub use caller::*;
pub use channel::*;
pub use codec::*;
pub use command::{
    StaticCStr, borrowed_cstr_to_str, borrowed_cstr_to_string, command_text, cstring, free_cstr,
    strdup_to_string,
};
pub use console::{
    CompletionFunc, CompletionMatches, complete, execute, execute_api, expand_alias, free_matches,
};
pub use core::{
    get_domain, get_hostname, get_switchname, get_uuid, get_variable, hupall, hupall_endpoint,
    hupall_matching_var, hupall_matching_vars, session_count, sessions_per_second, set_variable,
    uptime,
};
pub use core_db::{CacheDbType, CoreDb, Stmt, StmtRows, TableRows, cache_db_type};
pub use endpoint::{
    Dtmf, DtmfSource, EndpointInterfaceRef, EndpointIoBuilder, EndpointIoRoutines, Frame, FrameMut,
    IoFlags, IoRoutinesBuilder, MessageType, OutgoingResult, SessionMessage, StateHandlerTable,
    request_session,
};
pub use estimators::{CusumDetector, KalmanEstimator, is_slow_link};
pub use event::{
    Event, EventBinder, EventRef, EventType, EventXml, HeaderIter, Priority, binary_deserialize,
    bind_permanent, channel_bind, channel_broadcast, channel_deliver, channel_permission_clear,
    channel_permission_modify, channel_permission_verify, channel_unbind, event_name,
    event_running, name_event, unbind_callback,
};
pub use file::*;
pub use ivr::*;
pub use jitterbuffer::{JbFlag, JbFrames, JbKind, JitterBuffer, JitterBufferConfig};
pub use limit::{
    Usage, backend, fire_event, incr, init, interval_reset, release, reset, status, usage,
};
pub use logging::*;
pub use media::*;
pub use module::{
    ApiInterface, ApplicationInfo, ApplicationInterface, AsrCloseFn, AsrFeedFn, AsrInterface,
    AsrLoadGrammarFn, AsrOpenFn, AsrUnloadGrammarFn, ChatApplicationInterface, ChatInterface,
    ChatSendFn, DatabaseInterface, DbExecDetailedFn, DbHandleDestroyFn, DbHandleNewFn,
    DialplanInterface, DirectoryCloseFn, DirectoryInterface, DirectoryNextFn, DirectoryNextPairFn,
    DirectoryOpenFn, DirectoryQueryFn, EndpointInterface, FileCloseFn, FileInterface, FileOpenFn,
    FileReadFn, FileTruncateFn, FileWriteFn, JsonApiInterface, LimitIncrFn, LimitInterface,
    LimitIntervalResetFn, LimitReleaseFn, LimitResetFn, LimitStatusFn, LimitUsageFn, ManagementFn,
    ManagementInterface, Module, ModuleBuilder, SayInterface, SpeechCloseFn, SpeechFeedTtsFn,
    SpeechInterface, SpeechOpenFn, SpeechReadTtsFn, TimerCheckFn, TimerDestroyFn, TimerInitFn,
    TimerInterface, TimerNextFn, TimerStepFn, TimerSyncFn,
};
pub use nat::{
    NatIpProto, add_mapping as nat_add_mapping, del_mapping as nat_del_mapping, init as nat_init,
    is_initialized as nat_is_initialized, late_init as nat_late_init, reinit as nat_reinit,
    republish as nat_republish, set_mapping as nat_set_mapping, shutdown as nat_shutdown,
    type_str as nat_type_str,
};
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
    status_to_result, switch_bool,
};
pub use stream::*;
pub use timer::Timer;
pub use utils::{escape_string, find_end_paren, format_number, url_encode};
pub use vad::{SpeechSegment, Vad, VadState, snap_segments};
pub use video::{
    CachedImage, Chromakey, Color, Image, ImageFit, ImageFormat, ImagePosition, Shade,
};
pub use xml::*;

#[macro_export]
macro_rules! api_callback {
    (fn $name:ident($cmd:ident, $session:ident, $stream:ident) $body:block) => {
        // FFI boundary: returns `sys::switch_status_t` (raw). The user's `$body` runs in an
        // inner closure that returns `fswtch::Status`; early `return Status::X` inside the
        // body returns from the closure, and `.raw()` translates it here.
        // The whole body — including the `from_raw` conversions and the user's `$body` — is
        // wrapped in `catch_unwind` so a panic cannot unwind across the `unsafe extern "C"`
        // boundary into FreeSWITCH (which is UB / aborts the whole FS process). On panic we
        // log via `fswtch::log_error` and return `Status::GENERR`. This relies on the
        // downstream crate being built with `panic = "unwind"` (the default; the mod_* crates
        // set it explicitly).
        unsafe extern "C" fn $name(
            cmd_raw: *const ::std::ffi::c_char,
            session_raw: *mut $crate::sys::switch_core_session_t,
            stream_raw: *mut $crate::sys::switch_stream_handle_t,
        ) -> $crate::sys::switch_status_t {
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
                ::std::result::Result::Ok(status) => status.raw(),
                ::std::result::Result::Err(panic) => {
                    $crate::log_error(
                        "fswtch",
                        ::std::format!("panic in api callback {}: {:?}", stringify!($name), panic),
                    );
                    $crate::Status::GENERR.raw()
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
        // return unit, so on panic we just log and return.
        unsafe extern "C" fn $name(
            $session: *mut $crate::sys::switch_core_session_t,
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
        // See `api_callback!` — returning `fswtch::Status`, wrapped in `catch_unwind` so a panic cannot unwind
        // across the `unsafe extern "C"` boundary into FreeSWITCH.
        unsafe extern "C" fn $name(
            event_raw: *mut $crate::sys::switch_event_t,
            data_raw: *const ::std::ffi::c_char,
        ) -> $crate::sys::switch_status_t {
            let result = ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| {
                let body =
                    |$event: $crate::EventRef, $data: Option<String>| -> $crate::Status { $body };
                let $event = unsafe { $crate::EventRef::from_raw(event_raw) };
                let $data = unsafe { $crate::command_text(data_raw) };
                body($event, $data)
            }));
            match result {
                ::std::result::Result::Ok(status) => status.raw(),
                ::std::result::Result::Err(panic) => {
                    $crate::log_error(
                        "fswtch",
                        ::std::format!("panic in chat callback {}: {:?}", stringify!($name), panic),
                    );
                    $crate::Status::GENERR.raw()
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
        // return unit, so on panic we just log and return.
        unsafe extern "C" fn $name($event: *mut $crate::sys::switch_event_t) {
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
        // Returns `sys::switch_status_t` (raw) at the FFI boundary; the user's `$body`
        // produces `fswtch::Result<ModuleBuilder>`, mapped to a `Status` and unwrapped to
        // its raw value here.
        unsafe extern "C" fn $name(
            module_interface: *mut *mut $crate::sys::switch_loadable_module_interface_t,
            pool: *mut $crate::sys::switch_memory_pool_t,
        ) -> $crate::sys::switch_status_t {
            let $module =
                match unsafe { $crate::ModuleBuilder::new(module_interface, pool, $module_name) } {
                    Ok(module) => module,
                    Err(error) => return error.0.raw(),
                };
            let result: $crate::Result<$crate::ModuleBuilder> = $body;
            match result {
                Ok(_) => $crate::Status::SUCCESS.raw(),
                Err(error) => error.0.raw(),
            }
        }
    };
}
