use std::ptr::NonNull;

use crate::caller::CallerProfile;
use crate::command::{borrowed_cstr_to_str, borrowed_cstr_to_string, strdup_to_string};
use crate::{Cause, Result, cstring, status_to_result, sys};

/// A borrowed handle to a FreeSWITCH channel — the per-call state machine, variable store, and
/// caller-profile owner.
///
/// Obtained via [`crate::Session::channel`]. The handle borrows the session it came from and must
/// not outlive it. `Channel` is `Copy`; pass it by value.
#[derive(Copy, Clone)]
pub struct Channel {
    raw: NonNull<sys::switch_channel_t>,
}

impl Channel {
    /// Wraps a FreeSWITCH channel pointer for the duration of a callback or borrowed access.
    ///
    /// # Safety
    ///
    /// `raw` must point to a live FreeSWITCH channel and remain valid while this wrapper is used.
    pub unsafe fn from_raw(raw: *mut sys::switch_channel_t) -> Option<Self> {
        NonNull::new(raw).map(|raw| Self { raw })
    }

    #[inline]
    pub fn as_ptr(self) -> *mut sys::switch_channel_t {
        self.raw.as_ptr()
    }

    /// Reads a channel variable into an owned `String`.
    ///
    /// Uses `switch_channel_get_variable_strdup`, which returns a freshly malloc'd copy (no memory
    /// pool) that this method frees after copying. The result does not borrow the channel and is not
    /// invalidated by later `set_variable` calls. Returns `Ok(None)` when the variable is unset.
    pub fn variable(self, name: impl AsRef<str>) -> Result<Option<String>> {
        let name = cstring(name)?;
        // SAFETY: `self.raw` is a live channel; `name` is a valid C string for the call. The
        // returned pointer is null or a malloc'd "strdup copy ... without using a memory pool"
        // (per switch_channel.h) that `strdup_to_string` copies out and frees.
        let value =
            unsafe { sys::switch_channel_get_variable_strdup(self.raw.as_ptr(), name.as_ptr()) };
        // SAFETY: `value` is null or a malloc'd C string as above.
        Ok(unsafe { strdup_to_string(value.cast_mut()) })
    }

    /// Sets a channel variable, substituting it into the channel's variable scope.
    pub fn set_variable(self, name: impl AsRef<str>, value: &str) -> Result<()> {
        let name = cstring(name)?;
        let value = cstring(value)?;
        // SAFETY: `self.raw` is a live channel; both C strings are valid for the call.
        let status = unsafe {
            sys::switch_channel_set_variable_var_check(
                self.raw.as_ptr(),
                name.as_ptr(),
                value.as_ptr(),
                sys::switch_bool_t_SWITCH_TRUE,
            )
        };
        status_to_result(status)
    }

    /// The channel's display name (e.g. `"sofia/internal/1001@..."`).
    pub fn name(self) -> Option<String> {
        // SAFETY: `self.raw` is a live channel.
        let ptr = unsafe { sys::switch_channel_get_name(self.raw.as_ptr()) };
        borrowed_cstr_to_string(ptr.cast_const())
    }

    /// The channel's UUID.
    pub fn uuid(self) -> Option<String> {
        // SAFETY: `self.raw` is a live channel.
        let ptr = unsafe { sys::switch_channel_get_uuid(self.raw.as_ptr()) };
        borrowed_cstr_to_string(ptr.cast_const())
    }

    /// The channel's current state (`CS_*`).
    pub fn state(self) -> sys::switch_channel_state_t {
        // SAFETY: `self.raw` is a live channel.
        unsafe { sys::switch_channel_get_state(self.raw.as_ptr()) }
    }

    /// The hangup cause recorded on the channel.
    pub fn cause(self) -> Cause {
        // SAFETY: `self.raw` is a live channel.
        unsafe { sys::switch_channel_get_cause(self.raw.as_ptr()) }
    }

    /// The caller profile attached to this channel.
    pub fn caller_profile(self) -> Option<CallerProfile> {
        // SAFETY: `self.raw` is a live channel.
        let raw = unsafe { sys::switch_channel_get_caller_profile(self.raw.as_ptr()) };
        // SAFETY: The profile borrows the channel and is live while `self` is.
        unsafe { CallerProfile::from_raw(raw) }
    }

    /// Returns `true` when `flag` (`CF_*`) is set on the channel.
    pub fn test_flag(self, flag: sys::switch_channel_flag_t) -> bool {
        // SAFETY: `self.raw` is a live channel.
        let set = unsafe { sys::switch_channel_test_flag(self.raw.as_ptr(), flag) };
        set != 0
    }

    /// Blocks the caller until the channel reaches `want` state. A null `other_channel` is passed so
    /// only this channel's own state is awaited.
    pub fn wait_for_state(self, want: sys::switch_channel_state_t) {
        // SAFETY: `self.raw` is a live channel; a null `other_channel` is permitted.
        unsafe {
            sys::switch_channel_wait_for_state(self.raw.as_ptr(), std::ptr::null_mut(), want)
        };
    }

    /// Requests a state transition. Returns the resulting state.
    pub fn set_state(self, state: sys::switch_channel_state_t) -> sys::switch_channel_state_t {
        // SAFETY: `self.raw` is a live channel; source strings are static C strings.
        unsafe {
            sys::switch_channel_perform_set_state(
                self.raw.as_ptr(),
                c"fswtch-rs".as_ptr(),
                c"Channel::set_state".as_ptr(),
                line!() as _,
                state,
            )
        }
    }

    /// Hangs up the channel with the given cause. Returns the resulting state.
    pub fn hangup(self, cause: Cause) -> sys::switch_channel_state_t {
        // SAFETY: `self.raw` is a live channel; source strings are static C strings.
        unsafe {
            sys::switch_channel_perform_hangup(
                self.raw.as_ptr(),
                c"fswtch-rs".as_ptr(),
                c"Channel::hangup".as_ptr(),
                line!() as _,
                cause,
            )
        }
    }
}

/// Translates a cause name (e.g. `"normal_clearing"`) into a [`Cause`].
pub fn str_to_cause(name: impl AsRef<str>) -> Result<Cause> {
    let name = cstring(name)?;
    // SAFETY: `name` is a valid C string for the call.
    Ok(unsafe { sys::switch_channel_str2cause(name.as_ptr()) })
}

/// Translates a [`Cause`] into its canonical name. The returned string borrows static storage.
pub fn cause_to_str(cause: Cause) -> Option<&'static str> {
    // SAFETY: `switch_channel_cause2str` returns a static string literal.
    let ptr = unsafe { sys::switch_channel_cause2str(cause) };
    // SAFETY: `ptr` is null or a static null-terminated string.
    unsafe { borrowed_cstr_to_str(ptr) }
}
