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
mod event;
mod exports;
mod ivr;
mod jitterbuffer;
mod limit;
mod logging;
mod media;
mod module;
mod pool;
mod regex;
mod resample;
mod rtp;
mod scheduler;
mod session;
mod status;
mod stream;
mod timer;
mod utils;
mod vad;
mod video;
mod xml;

pub use fswtch_sys as sys;

pub use buffer::Buffer;
pub use caller::CallerProfile;
pub use channel::{Channel, cause_to_str, str_to_cause};
pub use codec::Codec;
pub use command::{StaticCStr, borrowed_cstr_to_str, borrowed_cstr_to_string, command_text, cstring, free_cstr, strdup_to_string};
pub use console::{CompletionFunc, CompletionMatches, complete, execute, expand_alias, free_matches};
pub use core::{get_domain, get_hostname, get_switchname, get_uuid, get_variable, set_variable};
pub use core_db::{CoreDb, Stmt, StmtRows};
pub use endpoint::{Dtmf, DtmfSource, Frame, FrameMut, IoFlags, IoRoutinesBuilder, SessionMessage};
pub use event::{Event, EventBinder, EventRef};
pub use ivr::{park, record_file};
pub use jitterbuffer::{
    JbFlag, JbFrames, JbKind, JitterBuffer, JitterBufferConfig,
};
pub use limit::{Usage, backend, fire_event, incr, init, interval_reset, release, reset, status, usage};
pub use logging::{
    LogLevel, log, log_alert, log_console, log_critical, log_debug, log_debug1, log_debug2,
    log_debug3, log_debug4, log_debug5, log_debug6, log_debug7, log_debug8, log_debug9,
    log_debug10, log_error, log_example, log_example_error, log_info, log_notice, log_warning,
};
pub use media::{
    MediaBug, MediaBugAction, MediaBugConfig, MediaBugContext, MediaBugFlags, MediaBugHandler,
    MediaFrame, MediaFrameMut, attach_media_bug,
};
pub use module::{
    ApiInterface, ApplicationInfo, ApplicationInterface, ChatApplicationInterface,
    EndpointInterface, Module, ModuleBuilder,
};
pub use pool::Pool;
pub use regex::{CaptureCallback, Regex, RegexMatch, is_match, is_match_partial};
pub use resample::{
    Agc, AgcConfig, DEFAULT_QUALITY, Resample, calc_buffer_size, change_sln_volume,
    change_sln_volume_granular, char_to_float, float_to_char, float_to_short,
    generate_sln_silence, merge_sln, mux_channels, short_to_float, swap_linear, unmerge_sln,
};
pub use rtp::{Rtp, RtpConfig, request_port};
pub use scheduler::{
    Task, TaskConfig, TaskFlags, TaskHandle, TaskHandler, cancel_group, spawn, start, stop,
};
pub use session::{Session, SessionGuard};
pub use status::{
    CAUSE_NONE, CAUSE_NORMAL_CLEARING, CAUSE_NO_ANSWER, CAUSE_NO_USER_RESPONSE,
    CAUSE_ORIGINATOR_CANCEL, CAUSE_RECOVERY_ON_TIMER_EXPIRE, CAUSE_USER_BUSY, FALSE, GENERR,
    Cause, Result, SUCCESS, Status, SwitchError, false_on_success, status_to_result,
};
pub use stream::{ApiStream, Stream, write_stream_response};
pub use timer::Timer;
pub use utils::{escape_string, find_end_paren, format_number, url_encode};
pub use vad::{Vad, VadState};
pub use video::{Chromakey, Color, Image, ImageFormat};
pub use xml::{XmlConfig, XmlNode};

#[macro_export]
macro_rules! api_callback {
    (fn $name:ident($cmd:ident, $session:ident, $stream:ident) $body:block) => {
        unsafe extern "C" fn $name(
            $cmd: *const ::std::ffi::c_char,
            $session: *mut $crate::sys::switch_core_session_t,
            $stream: *mut $crate::sys::switch_stream_handle_t,
        ) -> $crate::Status {
            let $cmd = unsafe { $crate::command_text($cmd) };
            let $session = unsafe { $crate::Session::from_raw($session) };
            let $stream = unsafe { $crate::ApiStream::from_raw($stream) };
            $body
        }
    };
}

#[macro_export]
macro_rules! app_callback {
    (fn $name:ident($session:ident, $data:ident) $body:block) => {
        unsafe extern "C" fn $name(
            $session: *mut $crate::sys::switch_core_session_t,
            $data: *const ::std::ffi::c_char,
        ) {
            let $session = unsafe { $crate::Session::from_raw($session) };
            let $data = unsafe { $crate::command_text($data) };
            $body
        }
    };
}

#[macro_export]
macro_rules! chat_callback {
    (fn $name:ident($event:ident, $data:ident) $body:block) => {
        unsafe extern "C" fn $name(
            $event: *mut $crate::sys::switch_event_t,
            $data: *const ::std::ffi::c_char,
        ) -> $crate::Status {
            let $event = unsafe { $crate::EventRef::from_raw($event) };
            let $data = unsafe { $crate::command_text($data) };
            $body
        }
    };
}

/// Declares an `unsafe extern "C" fn` matching FreeSWITCH's `switch_event_callback_t`, wrapping the
/// raw event pointer in an [`EventRef`](crate::EventRef) for a safe body. Use the resulting function
/// pointer with [`EventBinder::bind`](crate::EventBinder::bind).
#[macro_export]
macro_rules! event_callback {
    (fn $name:ident($event:ident) $body:block) => {
        unsafe extern "C" fn $name($event: *mut $crate::sys::switch_event_t) {
            let $event = unsafe { $crate::EventRef::from_raw($event) };
            $body
        }
    };
}

#[macro_export]
macro_rules! module_load {
    (fn $name:ident($module:ident) for $module_name:literal $body:block) => {
        unsafe extern "C" fn $name(
            module_interface: *mut *mut $crate::sys::switch_loadable_module_interface_t,
            pool: *mut $crate::sys::switch_memory_pool_t,
        ) -> $crate::Status {
            let $module =
                match unsafe { $crate::ModuleBuilder::new(module_interface, pool, $module_name) } {
                    Ok(module) => module,
                    Err(error) => return error.0,
                };
            let result: $crate::Result<$crate::ModuleBuilder> = $body;
            match result {
                Ok(_) => $crate::SUCCESS,
                Err(error) => error.0,
            }
        }
    };
}
