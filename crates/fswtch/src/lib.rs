#![allow(clippy::not_unsafe_ptr_arg_deref)]

mod command;
mod event;
mod exports;
mod logging;
mod media;
mod module;
mod session;
mod status;
mod stream;

pub use fswtch_sys as sys;

pub use command::{command_text, cstring};
pub use event::{Event, EventRef};
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
    ApiInterface, ApplicationInterface, ChatApplicationInterface, EndpointInterface, Module,
    ModuleBuilder,
};
pub use session::Session;
pub use status::{
    FALSE, GENERR, Result, SUCCESS, Status, SwitchError, false_on_success, status_to_result,
};
pub use stream::{ApiStream, Stream, write_stream_response};

#[macro_export]
macro_rules! api_callback {
    (fn $name:ident($cmd:ident, $session:ident, $stream:ident) $body:block) => {
        unsafe extern "C" fn $name(
            $cmd: *const ::std::ffi::c_char,
            $session: *mut $crate::sys::switch_core_session_t,
            $stream: *mut $crate::sys::switch_stream_handle_t,
        ) -> $crate::Status {
            let $cmd = $crate::command_text($cmd);
            let $session = $crate::Session::from_raw($session);
            let $stream = $crate::ApiStream::from_raw($stream);
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
            let $session = $crate::Session::from_raw($session);
            let $data = $crate::command_text($data);
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
            let $event = $crate::EventRef::from_raw($event);
            let $data = $crate::command_text($data);
            $body
        }
    };
}
