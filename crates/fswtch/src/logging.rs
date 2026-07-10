use std::{ffi::CString, fmt};

use crate::sys;

macro_rules! call_ffi {
    ($call:expr) => {{
        // SAFETY: The caller documents the FreeSWITCH ABI preconditions at each call site.
        unsafe { $call }
    }};
}

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

#[inline]
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
    unsafe { log_printf(level, text.as_ptr()) };
}

/// # Safety
///
/// `text` must point to a live null-terminated C string for this varargs call.
// SAFETY: The caller must pass a live null-terminated message pointer.
unsafe fn log_printf(level: sys::switch_log_level_t, text: *const std::ffi::c_char) {
    let log = sys::switch_log_printf;
    let channel = sys::switch_text_channel_t_SWITCH_CHANNEL_ID_LOG;
    call_ffi!(log(
        channel,
        c"fswtch-rs".as_ptr(),
        c"log".as_ptr(),
        line!() as _,
        std::ptr::null(),
        level,
        c"%s\n".as_ptr(),
        text,
    ));
}

// ── log level conversion + logger bind/unbind ─────────────────────────────

pub fn log_level2str(level: crate::sys::switch_log_level_t) -> Option<&'static str> {
    // SAFETY: returns null or a static string.
    let ptr = unsafe { crate::sys::switch_log_level2str(level) };
    unsafe { crate::borrowed_cstr_to_str(ptr) }
}

pub fn log_str2level(s: impl AsRef<str>) -> crate::Result<crate::sys::switch_log_level_t> {
    let s = crate::cstring(s)?;
    // SAFETY: valid C string.
    Ok(unsafe { crate::sys::switch_log_str2level(s.as_ptr()) })
}

pub fn log_str2mask(s: impl AsRef<str>) -> crate::Result<u32> {
    let s = crate::cstring(s)?;
    // SAFETY: valid C string.
    Ok(unsafe { crate::sys::switch_log_str2mask(s.as_ptr()) })
}

pub fn log_bind_logger(
    function: crate::sys::switch_log_function_t,
    level: crate::sys::switch_log_level_t,
    is_console: bool,
) -> crate::Result<()> {
    let ic = if is_console {
        crate::sys::switch_bool_t_SWITCH_TRUE
    } else {
        crate::sys::switch_bool_t_SWITCH_FALSE
    };
    // SAFETY: valid fn ptr; valid level; valid bool.
    crate::status_to_result(unsafe { crate::sys::switch_log_bind_logger(function, level, ic) })
}

pub fn log_unbind_logger(function: crate::sys::switch_log_function_t) -> crate::Result<()> {
    // SAFETY: valid fn ptr.
    crate::status_to_result(unsafe { crate::sys::switch_log_unbind_logger(function) })
}
