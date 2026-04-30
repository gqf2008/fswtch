use std::{ffi::CStr, ptr::NonNull};

use crate::{Result, status_to_result, sys};

#[derive(Copy, Clone)]
pub struct Session {
    raw: NonNull<sys::switch_core_session_t>,
}

impl Session {
    pub fn from_raw(raw: *mut sys::switch_core_session_t) -> Option<Self> {
        NonNull::new(raw).map(|raw| Self { raw })
    }

    pub fn as_ptr(self) -> *mut sys::switch_core_session_t {
        self.raw.as_ptr()
    }

    pub fn answer(self) -> Result<()> {
        // SAFETY: `self.raw` is a live session pointer provided by FreeSWITCH.
        let channel = unsafe { sys::switch_core_session_get_channel(self.raw.as_ptr()) };
        let Some(channel) = NonNull::new(channel) else {
            return Ok(());
        };

        // SAFETY: `channel` belongs to `self.raw`; source strings are static C strings.
        let status = unsafe {
            sys::switch_channel_perform_answer(
                channel.as_ptr(),
                c"fswtch-rs".as_ptr(),
                c"Session::answer".as_ptr(),
                line!() as _,
            )
        };
        status_to_result(status)
    }

    pub fn sleep_ms(self, milliseconds: u32) -> Result<()> {
        // SAFETY: `self.raw` is a live session pointer provided by FreeSWITCH.
        let status = unsafe {
            sys::switch_ivr_sleep(
                self.raw.as_ptr(),
                milliseconds,
                sys::switch_bool_t_SWITCH_FALSE,
                std::ptr::null_mut(),
            )
        };
        status_to_result(status)
    }

    pub fn play_file(self, path: &CStr) -> Result<()> {
        // SAFETY: `self.raw` is live and `path` is a valid C string for the duration of the call.
        let status = unsafe {
            sys::switch_ivr_play_file(
                self.raw.as_ptr(),
                std::ptr::null_mut(),
                path.as_ptr(),
                std::ptr::null_mut(),
            )
        };
        status_to_result(status)
    }
}
