use std::ffi::{CStr, CString, c_char};

use crate::{GENERR, Result, SwitchError};

/// Converts a nullable FreeSWITCH command pointer into a trimmed Rust string.
///
/// # Safety
///
/// When non-null, `cmd` must point to a valid null-terminated C string for the duration of this
/// call.
// SAFETY: The caller must supply either null or a valid, live, null-terminated C string pointer.
pub unsafe fn command_text(cmd: *const c_char) -> Option<String> {
    if cmd.is_null() {
        return None;
    }

    // SAFETY: Forwarded from `command_text`'s caller.
    unsafe { CStr::from_ptr(cmd) }
        .to_str()
        .ok()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}

pub fn cstring(text: impl AsRef<str>) -> Result<CString> {
    CString::new(text.as_ref()).map_err(|_| SwitchError(GENERR))
}

pub trait StaticCStr {
    fn into_static_cstr(self) -> Result<&'static CStr>;
}

impl StaticCStr for &'static CStr {
    fn into_static_cstr(self) -> Result<&'static CStr> {
        Ok(self)
    }
}

impl StaticCStr for &'static str {
    fn into_static_cstr(self) -> Result<&'static CStr> {
        let text = cstring(self)?;
        Ok(Box::leak(text.into_boxed_c_str()))
    }
}
