#![allow(clippy::not_unsafe_ptr_arg_deref)]

mod exports;
mod logging;
mod module;
mod status;
mod stream;

pub use fswtch_sys as sys;

pub use logging::{
    LogLevel, log, log_alert, log_console, log_critical, log_debug, log_debug1, log_debug2,
    log_debug3, log_debug4, log_debug5, log_debug6, log_debug7, log_debug8, log_debug9,
    log_debug10, log_error, log_example, log_example_error, log_info, log_notice, log_warning,
};
pub use module::{ApiInterface, Module};
pub use status::{FALSE, GENERR, Result, SUCCESS, Status, SwitchError, status_to_result};
pub use stream::Stream;
