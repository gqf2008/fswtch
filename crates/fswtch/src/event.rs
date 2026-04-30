use std::{ffi::CStr, ptr::NonNull};

use crate::{Result, cstring, status_to_result, sys};

pub struct Event {
    raw: Option<NonNull<sys::switch_event_t>>,
}

impl Event {
    pub fn custom(subclass: impl AsRef<str>) -> Result<Self> {
        let subclass = cstring(subclass)?;
        let mut raw = std::ptr::null_mut();
        // SAFETY: FreeSWITCH initializes `raw` for the custom subclass when the call succeeds.
        let status = unsafe {
            sys::switch_event_create_subclass_detailed(
                c"fswtch-rs".as_ptr(),
                c"Event::custom".as_ptr(),
                line!() as _,
                &mut raw,
                sys::switch_event_types_t::SWITCH_EVENT_CUSTOM,
                subclass.as_ptr(),
            )
        };
        status_to_result(status)?;
        Ok(Self {
            raw: NonNull::new(raw),
        })
    }

    pub fn as_ptr(&self) -> *mut sys::switch_event_t {
        self.raw.map_or(std::ptr::null_mut(), NonNull::as_ptr)
    }

    pub fn add_header(&mut self, name: impl AsRef<str>, value: &str) -> Result<()> {
        let Some(raw) = self.raw else {
            return Ok(());
        };
        let name = cstring(name)?;
        let value = cstring(value)?;
        // SAFETY: `raw` is a live event and both C strings are valid for this call.
        let status = unsafe {
            sys::switch_event_add_header_string(
                raw.as_ptr(),
                sys::switch_stack_t::SWITCH_STACK_BOTTOM,
                name.as_ptr(),
                value.as_ptr(),
            )
        };
        status_to_result(status)
    }

    pub fn add_header_name(&mut self, name: impl AsRef<str>, value: &str) -> Result<()> {
        self.add_header(name, value)
    }

    pub fn fire(mut self) -> Result<()> {
        let Some(raw) = self.raw.take() else {
            return Ok(());
        };
        let mut raw = raw.as_ptr();
        // SAFETY: Ownership of `raw` transfers to FreeSWITCH on successful fire.
        let status = unsafe {
            sys::switch_event_fire_detailed(
                c"fswtch-rs".as_ptr(),
                c"Event::fire".as_ptr(),
                line!() as _,
                &mut raw,
                std::ptr::null_mut(),
            )
        };
        if status == crate::SUCCESS {
            Ok(())
        } else {
            self.raw = NonNull::new(raw);
            Err(crate::SwitchError(status))
        }
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        if let Some(raw) = self.raw.take() {
            let mut raw = raw.as_ptr();
            // SAFETY: The event is still owned by this wrapper because `fire` was not called.
            unsafe {
                sys::switch_event_destroy(&mut raw);
            }
        }
    }
}

#[derive(Copy, Clone)]
pub struct EventRef {
    raw: Option<NonNull<sys::switch_event_t>>,
}

impl EventRef {
    /// Wraps a FreeSWITCH event pointer for the duration of a callback.
    ///
    /// # Safety
    ///
    /// `raw` must point to a live FreeSWITCH event and remain valid while this wrapper is used.
    pub unsafe fn from_raw(raw: *mut sys::switch_event_t) -> Self {
        Self {
            raw: NonNull::new(raw),
        }
    }

    pub fn as_ptr(self) -> *mut sys::switch_event_t {
        self.raw.map_or(std::ptr::null_mut(), NonNull::as_ptr)
    }

    pub fn header(self, name: impl AsRef<str>) -> Option<String> {
        let raw = self.raw?;
        let name = cstring(name).ok()?;
        // SAFETY: `raw` is a live event for the callback duration.
        let value = unsafe { sys::switch_event_get_header_idx(raw.as_ptr(), name.as_ptr(), -1) };
        if value.is_null() {
            return None;
        }

        // SAFETY: FreeSWITCH returns a null-terminated header value when present.
        unsafe { CStr::from_ptr(value) }
            .to_str()
            .ok()
            .map(ToOwned::to_owned)
    }
}
