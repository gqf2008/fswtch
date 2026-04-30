use std::ffi::{CStr, CString, c_char};

use crate::{GENERR, Result, SwitchError};

pub fn command_text(cmd: *const c_char) -> Option<String> {
    if cmd.is_null() {
        return None;
    }

    // SAFETY: FreeSWITCH passes a null-terminated command string when one is present.
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
