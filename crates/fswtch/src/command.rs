use std::ffi::{CStr, CString, c_char, c_void};

use crate::{GENERR, Result, SwitchError};

unsafe extern "C" {
    fn free(ptr: *mut c_void);
}

/// Borrows a nullable FreeSWITCH C string as a Rust `&str` for the pointer's validity window.
///
/// No allocation. Use for values that borrow pool/channel/event storage (e.g. the pointer returned
/// by `switch_channel_get_variable_dup(.., SWITCH_FALSE, ..)` or `switch_event_get_header_idx`).
///
/// # Safety
///
/// When non-null, `ptr` must point to a valid null-terminated C string that remains valid for the
/// duration the returned `&str` is used.
// SAFETY: The caller must supply either null or a valid, live, null-terminated C string pointer.
pub unsafe fn borrowed_cstr_to_str<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: Forwarded from `borrowed_cstr_to_str`'s caller.
    unsafe { CStr::from_ptr(ptr) }.to_str().ok()
}

/// Copies a nullable FreeSWITCH C string into an owned Rust `String`.
pub fn borrowed_cstr_to_string(ptr: *const c_char) -> Option<String> {
    // SAFETY: `ptr` is only read here; the caller guarantees it is null or a valid C string.
    unsafe { borrowed_cstr_to_str(ptr) }.map(ToOwned::to_owned)
}

/// Frees a malloc'd C string previously returned by a FreeSWITCH `*_strdup` function
/// (e.g. `switch_channel_get_variable_strdup`).
///
/// # Safety
///
/// `ptr` must be null or a pointer obtained from a libc `malloc`-family allocator, and must not
/// have been freed already.
// SAFETY: The caller must provide null or a valid malloc'd pointer.
pub unsafe fn free_cstr(ptr: *mut c_char) {
    if !ptr.is_null() {
        // SAFETY: Guarded against null above; caller guarantees the pointer is malloc'd.
        unsafe { free(ptr.cast()) };
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cstring_round_trip() {
        let c = cstring("hello").unwrap();
        assert_eq!(c.to_str().unwrap(), "hello");
    }

    #[test]
    fn cstring_rejects_interior_nul() {
        assert!(cstring("a\0b").is_err());
    }

    #[test]
    fn borrowed_cstr_handles_null_and_value() {
        assert!(borrowed_cstr_to_string(std::ptr::null()).is_none());
        let c = cstring("world").unwrap();
        assert_eq!(borrowed_cstr_to_string(c.as_ptr()), Some("world".to_owned()));
    }

    #[test]
    fn static_cstr_from_static_str() {
        let s: &'static str = "literal";
        let c: &'static CStr = s.into_static_cstr().unwrap();
        assert_eq!(c.to_str().unwrap(), "literal");
    }
}
