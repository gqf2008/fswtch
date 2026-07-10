use std::ptr::NonNull;

use crate::command::borrowed_cstr_to_string;
use crate::{Result, cstring, sys};

/// A borrowed handle to a FreeSWITCH caller profile — the identity bag (ANI/DNIS/RDNIS, caller id,
/// source, context, ...) attached to a channel.
///
/// Obtained via [`crate::Channel::caller_profile`]. Borrows the channel it came from.
#[derive(Copy, Clone)]
pub struct CallerProfile {
    raw: NonNull<sys::switch_caller_profile_t>,
}

impl CallerProfile {
    /// Wraps a FreeSWITCH caller-profile pointer for borrowed access.
    ///
    /// # Safety
    ///
    /// `raw` must point to a live caller profile and remain valid while this wrapper is used.
    pub unsafe fn from_raw(raw: *mut sys::switch_caller_profile_t) -> Option<Self> {
        NonNull::new(raw).map(|raw| Self { raw })
    }

    #[inline]
    pub fn as_ptr(self) -> *mut sys::switch_caller_profile_t {
        self.raw.as_ptr()
    }

    /// Reads a profile field by name (e.g. `"username"`, `"caller_id_number"`, `"ani"`, `"rdnis"`,
    /// `"source"`, `"context"`, `"destination_number"`). Returns `Ok(None)` when the field is unset.
    pub fn field(self, name: impl AsRef<str>) -> Result<Option<String>> {
        let name = cstring(name)?;
        // SAFETY: `self.raw` is a live caller profile; `name` is a valid C string for the call.
        let value =
            unsafe { sys::switch_caller_get_field_by_name(self.raw.as_ptr(), name.as_ptr()) };
        Ok(borrowed_cstr_to_string(value))
    }
}

/// A borrowed handle to a FreeSWITCH caller extension — the ordered list of dialplan applications
/// (`extension_name`/`extension_number`) attached to a channel for execution.
///
/// Obtained via [`crate::Channel::caller_extension`] / [`crate::Channel::queued_extension`]. Borrows
/// the channel it came from. This is a `Copy` handle (like [`CallerProfile`]); clone freely.
#[derive(Copy, Clone)]
pub struct CallerExtension {
    raw: NonNull<sys::switch_caller_extension_t>,
}

impl CallerExtension {
    /// Wraps a FreeSWITCH caller-extension pointer for borrowed access.
    ///
    /// # Safety
    ///
    /// `raw` must be null or point to a live caller extension; the extension must remain valid
    /// while this wrapper is used.
    pub unsafe fn from_raw(raw: *mut sys::switch_caller_extension_t) -> Option<Self> {
        NonNull::new(raw).map(|raw| Self { raw })
    }

    #[inline]
    pub fn as_ptr(self) -> *mut sys::switch_caller_extension_t {
        self.raw.as_ptr()
    }

    /// The extension's name (`extension_name`), or `None` when unset.
    pub fn name(self) -> Option<String> {
        // SAFETY: `self.raw` is a live caller extension; reading the C field pointer is in-bounds.
        let ptr = unsafe { (*self.raw.as_ptr()).extension_name };
        borrowed_cstr_to_string(ptr)
    }

    /// The extension's number (`extension_number`), or `None` when unset.
    pub fn number(self) -> Option<String> {
        // SAFETY: `self.raw` is a live caller extension; reading the C field pointer is in-bounds.
        let ptr = unsafe { (*self.raw.as_ptr()).extension_number };
        borrowed_cstr_to_string(ptr)
    }

    /// The name of the currently executing application (`current_application.application_name`),
    /// or `None` when there is no current application.
    pub fn current_application_name(self) -> Option<String> {
        // SAFETY: `self.raw` is a live caller extension; reading `current_application` is in-bounds.
        let app = unsafe { (*self.raw.as_ptr()).current_application };
        if app.is_null() {
            return None;
        }
        // SAFETY: `app` was just checked non-null and points to a live caller application.
        let name = unsafe { (*app).application_name };
        borrowed_cstr_to_string(name)
    }

    /// The data argument of the currently executing application
    /// (`current_application.application_data`), or `None` when there is no current application.
    pub fn current_application_data(self) -> Option<String> {
        // SAFETY: `self.raw` is a live caller extension; reading `current_application` is in-bounds.
        let app = unsafe { (*self.raw.as_ptr()).current_application };
        if app.is_null() {
            return None;
        }
        // SAFETY: `app` was just checked non-null and points to a live caller application.
        let data = unsafe { (*app).application_data };
        borrowed_cstr_to_string(data)
    }
}

#[cfg(test)]
mod caller_extension_tests {
    use super::*;

    #[test]
    fn from_raw_null_is_none() {
        // SAFETY: null is the explicitly-allowed input.
        let got = unsafe { CallerExtension::from_raw(::std::ptr::null_mut()) };
        assert!(got.is_none());
    }

    #[test]
    fn as_ptr_round_trips_a_live_pointer() {
        let mut ext: sys::switch_caller_extension = unsafe { ::std::mem::zeroed() };
        // SAFETY: `raw` points to a stack-local live extension for the duration of the call; the
        // struct stays zeroed (no current application), so the getters read null fields.
        let raw: *mut sys::switch_caller_extension = &raw mut ext;
        let wrapped = unsafe { CallerExtension::from_raw(raw) }.expect("non-null raw should wrap");
        assert_eq!(wrapped.as_ptr(), raw);
    }

    #[test]
    fn getters_walk_a_populated_extension() {
        // Build one extension + current application with populated string fields, then assert the
        // safe getters copy the C strings out. No FreeSWITCH runtime is required.
        let name = std::ffi::CString::new("default").unwrap();
        let number = std::ffi::CString::new("1000").unwrap();
        let app_name = std::ffi::CString::new("playback").unwrap();
        let app_data = std::ffi::CString::new("ivr/welcome.wav").unwrap();

        let app = sys::switch_caller_application {
            application_name: app_name.as_ptr() as *mut _,
            application_data: app_data.as_ptr() as *mut _,
            application_function: None,
            next: ::std::ptr::null_mut(),
        };
        let mut ext: sys::switch_caller_extension = unsafe { ::std::mem::zeroed() };
        ext.extension_name = name.as_ptr() as *mut _;
        ext.extension_number = number.as_ptr() as *mut _;
        // SAFETY: `app` is a stack local live for the assertions below; taking its address is sound.
        ext.current_application = &raw const app as *mut sys::switch_caller_application_t;

        let raw: *mut sys::switch_caller_extension = &raw mut ext;
        // SAFETY: `raw` points to the stack-local live extension for the duration of the calls.
        let wrapped = unsafe { CallerExtension::from_raw(raw) }.expect("non-null raw should wrap");
        assert_eq!(wrapped.name().as_deref(), Some("default"));
        assert_eq!(wrapped.number().as_deref(), Some("1000"));
        assert_eq!(
            wrapped.current_application_name().as_deref(),
            Some("playback")
        );
        assert_eq!(
            wrapped.current_application_data().as_deref(),
            Some("ivr/welcome.wav")
        );
    }

    #[test]
    fn current_application_getters_are_none_without_one() {
        let mut ext: sys::switch_caller_extension = unsafe { ::std::mem::zeroed() };
        // `current_application` stays null; the app getters must early-return None.
        let raw: *mut sys::switch_caller_extension = &raw mut ext;
        // SAFETY: `raw` points to the stack-local live extension for the duration of the calls.
        let wrapped = unsafe { CallerExtension::from_raw(raw) }.expect("non-null raw should wrap");
        assert_eq!(wrapped.current_application_name(), None);
        assert_eq!(wrapped.current_application_data(), None);
    }
}

// ── caller extension/profile helpers ──────────────────────────────────────

pub fn caller_extension_clone(
    new_ext: &mut *mut crate::sys::switch_caller_extension_t,
    orig: *mut crate::sys::switch_caller_extension_t,
    pool: &crate::pool::Pool,
) -> crate::Result<()> {
    // SAFETY: valid out-param; `orig` live; `pool.as_ptr()` live.
    crate::status_to_result(unsafe { crate::sys::switch_caller_extension_clone(new_ext, orig, pool.as_ptr()) })
}

pub fn caller_extension_add_application(
    session: crate::Session,
    ext: *mut crate::sys::switch_caller_extension_t,
    application_name: impl AsRef<str>,
    extra_data: impl AsRef<str>,
) -> crate::Result<()> {
    let name = crate::cstring(application_name)?;
    let extra = crate::cstring(extra_data)?;
    // SAFETY: live session; valid ext; two valid C strings.
    crate::status_to_result(unsafe {
        crate::sys::switch_caller_extension_add_application(session.as_ptr(), ext, name.as_ptr(), extra.as_ptr())
    })
}

pub fn caller_profile_dup(
    pool: &crate::pool::Pool,
    tocopy: *mut crate::sys::switch_caller_profile_t,
) -> *mut crate::sys::switch_caller_profile_t {
    // SAFETY: live pool; `tocopy` live per caller.
    unsafe { crate::sys::switch_caller_profile_dup(pool.as_ptr(), tocopy) }
}

pub fn caller_profile_clone(
    session: crate::Session,
    tocopy: *mut crate::sys::switch_caller_profile_t,
) -> *mut crate::sys::switch_caller_profile_t {
    // SAFETY: live session; `tocopy` live.
    unsafe { crate::sys::switch_caller_profile_clone(session.as_ptr(), tocopy) }
}

pub fn caller_profile_event_set_data(
    profile: *mut crate::sys::switch_caller_profile_t,
    prefix: impl AsRef<str>,
    event: *mut crate::sys::switch_event_t,
) {
    let prefix = match crate::cstring(prefix) {
        Ok(s) => s,
        Err(_) => return,
    };
    // SAFETY: valid profile; valid C string; `event` per caller.
    unsafe { crate::sys::switch_caller_profile_event_set_data(profile, prefix.as_ptr(), event) };
}
