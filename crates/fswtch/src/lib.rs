#![allow(clippy::not_unsafe_ptr_arg_deref)]

mod command;
mod exports;
mod logging;
mod media;
mod module;
mod session;
mod status;
mod stream;

pub use fswtch_sys as sys;

pub use command::command_text;
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
};
pub use session::Session;
pub use status::{FALSE, GENERR, Result, SUCCESS, Status, SwitchError, status_to_result};
pub use stream::Stream;
