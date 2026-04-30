use std::ptr::NonNull;

use crate::{GENERR, Result, SwitchError, status_to_result, sys};

pub struct Stream {
    raw: NonNull<sys::switch_stream_handle_t>,
}

impl Stream {
    pub fn from_raw(raw: *mut sys::switch_stream_handle_t) -> Option<Self> {
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
