use std::ptr::NonNull;

use crate::{FALSE, GENERR, Result, SUCCESS, Status, SwitchError, status_to_result, sys};

pub struct Stream {
    raw: NonNull<sys::switch_stream_handle_t>,
}

impl Stream {
    /// Wraps a FreeSWITCH stream pointer for the duration of an API callback.
    ///
    /// Generic over the pointee so trampolines can pass a pointee-erased `*mut c_void` (never
    /// naming a `sys` type) while internal call sites pass the real stream pointer type.
    ///
    /// # Safety
    ///
    /// `raw` must point to a live FreeSWITCH stream handle and remain valid while this wrapper is
    /// used.
    pub unsafe fn from_raw<T>(raw: *mut T) -> Option<Self> {
        NonNull::new(raw as *mut sys::switch_stream_handle_t).map(|raw| Self { raw })
    }

    pub(crate) fn as_ptr(&self) -> *mut sys::switch_stream_handle_t {
        self.raw.as_ptr()
    }

    pub fn write_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        let raw = self.raw.as_ptr();
        // SAFETY: `self.raw` is guaranteed valid by `Stream::from_raw`'s caller contract.
        let Some(write) = (unsafe { &*raw }).raw_write_function else {
            return Err(SwitchError(GENERR));
        };

        // SAFETY: FreeSWITCH's stream writer accepts the stream handle and a byte buffer valid for
        // the duration of the call.
        let status = unsafe { write(raw, bytes.as_ptr().cast_mut(), bytes.len()) };
        status_to_result(status)
    }

    pub fn write_str(&mut self, text: &str) -> Result<()> {
        self.write_bytes(text.as_bytes())
    }
}

/// Writes a string response to a raw FreeSWITCH stream handle.
///
/// # Safety
///
/// `raw` must point to a live FreeSWITCH stream handle and remain valid for the duration of this
/// call.
// SAFETY: The caller must provide a live FreeSWITCH stream handle for the full call duration.
pub(crate) unsafe fn write_stream_response(
    raw: *mut sys::switch_stream_handle_t,
    text: &str,
) -> Status {
    // SAFETY: Forwarded from `write_stream_response`'s caller.
    let Some(mut stream) = (unsafe { Stream::from_raw(raw) }) else {
        return FALSE;
    };

    match stream.write_str(text) {
        Ok(()) => SUCCESS,
        Err(error) => error.0,
    }
}

#[derive(Copy, Clone)]
pub struct ApiStream {
    raw: *mut sys::switch_stream_handle_t,
}

impl ApiStream {
    /// Wraps a FreeSWITCH stream pointer for the duration of an API callback.
    ///
    /// Returns `None` when `raw` is null (FreeSWITCH passes null when no stream
    /// is attached to the callback). Mirrors [`Stream::from_raw`]'s null handling
    /// so the `api_callback!` macro's `$stream: Option<ApiStream>` parameter
    /// type-checks at every call site.
    ///
    /// Generic over the pointee so trampolines can pass a pointee-erased `*mut c_void` (never
    /// naming a `sys` type) while the real stream pointer type is stored internally.
    ///
    /// # Safety
    ///
    /// A non-null `raw` must point to a live FreeSWITCH stream handle and remain
    /// valid while this wrapper is used.
    pub unsafe fn from_raw<T>(raw: *mut T) -> Option<Self> {
        if raw.is_null() {
            return None;
        }
        Some(Self {
            raw: raw as *mut sys::switch_stream_handle_t,
        })
    }

    pub(crate) fn as_ptr(self) -> *mut sys::switch_stream_handle_t {
        self.raw
    }

    pub fn write(self, text: &str) -> Status {
        // SAFETY: `ApiStream` is constructed from a live callback stream by `ApiStream::from_raw`.
        unsafe { write_stream_response(self.raw, text) }
    }
}

// ── stream system/file helpers ─────────────────────────────────────────────

pub(crate) fn stream_system(
    cmd: impl AsRef<str>,
    stream: *mut crate::sys::switch_stream_handle_t,
) -> i32 {
    let cmd = match crate::cstring(cmd) {
        Ok(s) => s,
        Err(_) => return -1,
    };
    // SAFETY: valid C string; `stream` per caller.
    unsafe { crate::sys::switch_stream_system(cmd.as_ptr(), stream) }
}

pub(crate) fn stream_system_fork(
    cmd: impl AsRef<str>,
    stream: *mut crate::sys::switch_stream_handle_t,
) -> i32 {
    let cmd = match crate::cstring(cmd) {
        Ok(s) => s,
        Err(_) => return -1,
    };
    // SAFETY: valid C string; `stream` per caller.
    unsafe { crate::sys::switch_stream_system_fork(cmd.as_ptr(), stream) }
}

pub(crate) fn stream_write_file_contents(
    stream: *mut crate::sys::switch_stream_handle_t,
    path: impl AsRef<str>,
) -> crate::Result<()> {
    let path = crate::cstring(path)?;
    // SAFETY: `stream` per caller; valid C string.
    crate::status_to_result(unsafe {
        crate::sys::switch_stream_write_file_contents(stream, path.as_ptr())
    })
}
