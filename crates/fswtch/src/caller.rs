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
