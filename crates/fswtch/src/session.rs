use std::ffi::c_char;
use std::ptr::NonNull;

use crate::{Result, cstring, status_to_result, sys};

#[derive(Copy, Clone)]
pub struct Session {
    raw: NonNull<sys::switch_core_session_t>,
}

impl Session {
    /// Wraps a FreeSWITCH session pointer for the duration of a callback.
    ///
    /// # Safety
    ///
    /// `raw` must point to a live FreeSWITCH session and remain valid while this wrapper is used.
    pub unsafe fn from_raw(raw: *mut sys::switch_core_session_t) -> Option<Self> {
        NonNull::new(raw).map(|raw| Self { raw })
    }

    #[inline]
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

    pub fn play_file(self, path: impl AsRef<str>) -> Result<()> {
        let path = cstring(path)?;
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

    /// The channel backing this session.
    pub fn channel(self) -> Option<crate::Channel> {
        // SAFETY: `self.raw` is a live session; its channel is live for the session's lifetime.
        let raw = unsafe { sys::switch_core_session_get_channel(self.raw.as_ptr()) };
        // SAFETY: The channel borrows the session and is live while `self` is.
        unsafe { crate::Channel::from_raw(raw) }
    }

    /// Hangs up the session's channel with the given cause.
    pub fn hangup(self, cause: crate::Cause) {
        // SAFETY: `self.raw` is a live session; its channel is live for the session's lifetime.
        let raw = unsafe { sys::switch_core_session_get_channel(self.raw.as_ptr()) };
        // SAFETY: The channel borrows the session and is live while `self` is.
        if let Some(channel) = unsafe { crate::Channel::from_raw(raw) } {
            channel.hangup(cause);
        }
    }

    /// Executes a dialplan application by name (e.g. `"playback"`, `"park"`) with the given argument
    /// string. Pass an empty `data` when the application takes none.
    pub fn execute_application(self, app: impl AsRef<str>, data: &str) -> Result<()> {
        let app = cstring(app)?;
        let data = cstring(data)?;
        // SAFETY: `self.raw` is a live session; both C strings are valid for the call; a null flags
        // pointer means the caller does not want the application flags back.
        let status = unsafe {
            sys::switch_core_session_execute_application_get_flags(
                self.raw.as_ptr(),
                app.as_ptr(),
                data.as_ptr(),
                std::ptr::null_mut(),
            )
        };
        status_to_result(status)
    }
}

/// RAII guard for a session looked up by UUID via `switch_core_session_perform_locate`.
///
/// The session is read-locked for the guard's lifetime; `switch_core_session_rwunlock` runs on drop.
/// The borrowed [`Session`] returned by [`session`](Self::session) must not outlive this guard.
pub struct SessionGuard {
    inner: Option<Session>,
}

impl SessionGuard {
    /// Looks up a session by UUID and read-locks it. Returns `Ok(None)` when no such session exists.
    pub fn locate(uuid: impl AsRef<str>) -> Result<Option<Self>> {
        let uuid = cstring(uuid)?;
        // SAFETY: `uuid` is a valid C string for the call.
        Ok(unsafe { Self::from_uuid(uuid.as_ptr()) })
    }

    /// # Safety
    ///
    /// `uuid` must be a valid null-terminated C string for the duration of the call.
    // SAFETY: The caller must supply a valid C string.
    unsafe fn from_uuid(uuid: *const c_char) -> Option<Self> {
        // SAFETY: `uuid` is a valid C string per the caller's contract.
        let raw = unsafe {
            sys::switch_core_session_perform_locate(
                uuid,
                c"fswtch-rs".as_ptr(),
                c"SessionGuard::locate".as_ptr(),
                line!() as _,
            )
        };
        // SAFETY: `raw` is a live, read-locked session when non-null.
        let session = unsafe { Session::from_raw(raw) }?;
        Some(Self {
            inner: Some(session),
        })
    }

    /// The read-locked session. The borrow is tied to this guard; do not let it outlive the guard.
    pub fn session(&self) -> Option<&Session> {
        self.inner.as_ref()
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        if let Some(session) = self.inner.take() {
            // SAFETY: `session.as_ptr()` is the read-locked session obtained from `perform_locate`.
            unsafe { sys::switch_core_session_rwunlock(session.as_ptr()) };
        }
    }
}
