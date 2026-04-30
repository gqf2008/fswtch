use std::ptr::NonNull;

use crate::{FALSE, GENERR, Result, SUCCESS, Status, SwitchError, status_to_result, sys};

pub struct Stream {
    raw: NonNull<sys::switch_stream_handle_t>,
}

impl Stream {
    /// Wraps a FreeSWITCH stream pointer for the duration of an API callback.
    ///
    /// # Safety
    ///
    /// `raw` must point to a live FreeSWITCH stream handle and remain valid while this wrapper is
    /// used.
    pub unsafe fn from_raw(raw: *mut sys::switch_stream_handle_t) -> Option<Self> {
        NonNull::new(raw).map(|raw| Self { raw })
    }

    pub fn as_ptr(&self) -> *mut sys::switch_stream_handle_t {
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
pub unsafe fn write_stream_response(raw: *mut sys::switch_stream_handle_t, text: &str) -> Status {
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
    /// # Safety
    ///
    /// `raw` must point to a live FreeSWITCH stream handle and remain valid while this wrapper is
    /// used.
    pub unsafe fn from_raw(raw: *mut sys::switch_stream_handle_t) -> Self {
        Self { raw }
    }

    pub fn as_ptr(self) -> *mut sys::switch_stream_handle_t {
        self.raw
    }

    pub fn write(self, text: &str) -> Status {
        // SAFETY: `ApiStream` is constructed from a live callback stream by `ApiStream::from_raw`.
        unsafe { write_stream_response(self.raw, text) }
    }
}
