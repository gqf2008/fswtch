use std::{ffi::CString, fmt};

use crate::sys;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum LogLevel {
    Disable,
    Console,
    Alert,
    Critical,
    Error,
    Warning,
    Notice,
    Info,
    Debug,
    Debug1,
    Debug2,
    Debug3,
    Debug4,
    Debug5,
    Debug6,
    Debug7,
    Debug8,
    Debug9,
    Debug10,
    Invalid,
    Uninit,
}

impl LogLevel {
    pub const fn as_raw(self) -> sys::switch_log_level_t {
        match self {
            Self::Disable => sys::switch_log_level_t_SWITCH_LOG_DISABLE,
            Self::Console => sys::switch_log_level_t_SWITCH_LOG_CONSOLE,
            Self::Alert => sys::switch_log_level_t_SWITCH_LOG_ALERT,
            Self::Critical => sys::switch_log_level_t_SWITCH_LOG_CRIT,
            Self::Error => sys::switch_log_level_t_SWITCH_LOG_ERROR,
            Self::Warning => sys::switch_log_level_t_SWITCH_LOG_WARNING,
            Self::Notice => sys::switch_log_level_t_SWITCH_LOG_NOTICE,
            Self::Info => sys::switch_log_level_t_SWITCH_LOG_INFO,
            Self::Debug => sys::switch_log_level_t_SWITCH_LOG_DEBUG,
            Self::Debug1 => sys::switch_log_level_t_SWITCH_LOG_DEBUG1,
            Self::Debug2 => sys::switch_log_level_t_SWITCH_LOG_DEBUG2,
            Self::Debug3 => sys::switch_log_level_t_SWITCH_LOG_DEBUG3,
            Self::Debug4 => sys::switch_log_level_t_SWITCH_LOG_DEBUG4,
            Self::Debug5 => sys::switch_log_level_t_SWITCH_LOG_DEBUG5,
            Self::Debug6 => sys::switch_log_level_t_SWITCH_LOG_DEBUG6,
            Self::Debug7 => sys::switch_log_level_t_SWITCH_LOG_DEBUG7,
            Self::Debug8 => sys::switch_log_level_t_SWITCH_LOG_DEBUG8,
            Self::Debug9 => sys::switch_log_level_t_SWITCH_LOG_DEBUG9,
            Self::Debug10 => sys::switch_log_level_t_SWITCH_LOG_DEBUG10,
            Self::Invalid => sys::switch_log_level_t_SWITCH_LOG_INVALID,
            Self::Uninit => sys::switch_log_level_t_SWITCH_LOG_UNINIT,
        }
    }
}

pub fn log(module: &str, level: LogLevel, message: impl fmt::Display) {
    log_at(level.as_raw(), module, message);
}

pub fn log_console(module: &str, message: impl fmt::Display) {
    log(module, LogLevel::Console, message);
}

pub fn log_alert(module: &str, message: impl fmt::Display) {
    log(module, LogLevel::Alert, message);
}

pub fn log_critical(module: &str, message: impl fmt::Display) {
    log(module, LogLevel::Critical, message);
}

pub fn log_error(module: &str, message: impl fmt::Display) {
    log(module, LogLevel::Error, message);
}

pub fn log_warning(module: &str, message: impl fmt::Display) {
    log(module, LogLevel::Warning, message);
}

pub fn log_notice(module: &str, message: impl fmt::Display) {
    log(module, LogLevel::Notice, message);
}

pub fn log_info(module: &str, message: impl fmt::Display) {
    log(module, LogLevel::Info, message);
}

pub fn log_debug(module: &str, message: impl fmt::Display) {
    log(module, LogLevel::Debug, message);
}

pub fn log_debug1(module: &str, message: impl fmt::Display) {
    log(module, LogLevel::Debug1, message);
}

pub fn log_debug2(module: &str, message: impl fmt::Display) {
    log(module, LogLevel::Debug2, message);
}

pub fn log_debug3(module: &str, message: impl fmt::Display) {
    log(module, LogLevel::Debug3, message);
}

pub fn log_debug4(module: &str, message: impl fmt::Display) {
    log(module, LogLevel::Debug4, message);
}

pub fn log_debug5(module: &str, message: impl fmt::Display) {
    log(module, LogLevel::Debug5, message);
}

pub fn log_debug6(module: &str, message: impl fmt::Display) {
    log(module, LogLevel::Debug6, message);
}

pub fn log_debug7(module: &str, message: impl fmt::Display) {
    log(module, LogLevel::Debug7, message);
}

pub fn log_debug8(module: &str, message: impl fmt::Display) {
    log(module, LogLevel::Debug8, message);
}

pub fn log_debug9(module: &str, message: impl fmt::Display) {
    log(module, LogLevel::Debug9, message);
}

pub fn log_debug10(module: &str, message: impl fmt::Display) {
    log(module, LogLevel::Debug10, message);
}

pub fn log_example(module: &str, message: impl fmt::Display) {
    log_info(module, message);
}

pub fn log_example_error(module: &str, message: impl fmt::Display) {
    log_error(module, message);
}

fn log_at(level: sys::switch_log_level_t, module: &str, message: impl fmt::Display) {
    let text = format!("[fswtch:{module}] {message}");
    let text = text.replace('\0', "\\0");
    let Ok(text) = CString::new(text) else {
        return;
    };

    // SAFETY: All C strings are valid for the duration of the varargs logging call.
    unsafe {
        sys::switch_log_printf(
            sys::switch_text_channel_t_SWITCH_CHANNEL_ID_LOG,
            c"fswtch-rs".as_ptr(),
            c"log".as_ptr(),
            line!() as _,
            std::ptr::null(),
            level,
            c"%s\n".as_ptr(),
            text.as_ptr(),
        );
    }
}
