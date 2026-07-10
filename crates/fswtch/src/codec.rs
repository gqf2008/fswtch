//! Codec initialization, encoding, and decoding.
//!
//! Wraps FreeSWITCH's `switch_core_codec_init_with_bitrate` / `switch_core_codec_encode` /
//! `switch_core_codec_decode` / `switch_core_codec_destroy` API. A [`Codec`] owns a
//! `switch_codec_t` struct (allocated by the caller, not opaque) and releases it on drop.

use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::ptr::NonNull;

use crate::{Result, cstring, status_to_result, sys};

/// An initialized FreeSWITCH codec handle.
///
/// Owns the backing `switch_codec_t` storage, which FreeSWITCH fills in during
/// `switch_core_codec_init_with_bitrate` and tears down via `switch_core_codec_destroy` when this
/// wrapper is dropped. The codec borrows the memory pool it was initialized against and must not
/// outlive it.
pub struct Codec {
    raw: NonNull<sys::switch_codec_t>,
    /// The sample rate passed to `init_with_bitrate`, reused as the decoded/encoded rate argument
    /// for `encode` / `decode` so callers don't have to supply it on every call.
    rate: u32,
    // `switch_codec_t` is not thread-safe; `encode`/`decode` mutate C state through `&self`.
    _marker: PhantomData<*const ()>,
}

impl Codec {
    /// Initializes a codec handle against `pool`.
    ///
    /// `implementation` is the codec module name (e.g. `"PCMU"`, `"L16"`). `rate` is the desired
    /// sample rate in Hz (0 for any), `ms` the packetization interval in milliseconds (0 for any),
    /// and `channels` the channel count (0 for any). `fmtp`, `modname`, `bitrate`, `flags`, and
    /// `codec_settings` are left null/zero; reach for the raw FFI via [`Codec::as_ptr`] if you need
    /// them.
    ///
    /// The returned codec borrows `pool` and must not outlive it.
    pub fn new(
        implementation: impl AsRef<str>,
        rate: u32,
        ms: u32,
        channels: u32,
        pool: &crate::pool::Pool,
    ) -> Result<Self> {
        let implementation = cstring(implementation)?;

        // SAFETY: `switch_codec_t` is a plain `#[repr(C)]` struct (see bindings.rs) with no
        // padding-only invariants; zero-initializing it is the same operation bindgen's own
        // `Default` impl performs. The struct is about to be handed to `init_with_bitrate`, which
        // populates every field FreeSWITCH uses.
        let mut raw: Box<sys::switch_codec_t> =
            Box::new(unsafe { MaybeUninit::<sys::switch_codec_t>::zeroed().assume_init() });

        // SAFETY: `raw` is a freshly zeroed codec struct; `implementation` is a valid C string;
        // `pool.as_ptr()` is a live memory pool owned by the caller. Null fmtp/modname/settings and
        // zero bitrate/flags are permitted by the FreeSWITCH contract (the `switch_core_codec_init`
        // macro forwards exactly these defaults).
        let status = unsafe {
            sys::switch_core_codec_init_with_bitrate(
                Box::as_mut(&mut raw),
                implementation.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                rate,
                ms as std::os::raw::c_int,
                channels as std::os::raw::c_int,
                0,
                0,
                std::ptr::null(),
                pool.as_ptr(),
            )
        };

        status_to_result(status).map(|()| {
            // SAFETY: `raw` was just box-allocated and zeroed; `init` succeeded so the struct is a
            // live, non-null codec handle for the lifetime of this wrapper.
            let raw = unsafe { NonNull::new_unchecked(Box::into_raw(raw)) };
            Self {
                raw,
                rate,
                _marker: PhantomData,
            }
        })
    }

    /// The raw codec handle, for advanced use with the FreeSWITCH codec FFI.
    pub fn as_ptr(&self) -> *mut sys::switch_codec_t {
        self.raw.as_ptr()
    }

    /// The sample rate this codec was initialized with (Hz), passed as the decoded/encoded rate to
    /// [`Codec::encode`] / [`Codec::decode`].
    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Encodes `src` (raw PCM) into `dst`, returning the number of bytes written.
    ///
    /// `dst` must be large enough to hold the encoded frame; on success its in-use length is written
    /// back by FreeSWITCH. A null `other_codec` is passed so only this codec's encoder runs.
    pub fn encode(&self, src: &[u8], dst: &mut [u8]) -> Result<usize> {
        let mut out_len: u32 = dst.len() as u32;
        let mut out_rate: u32 = 0;
        let mut flag: std::os::raw::c_uint = 0;
        // SAFETY: `self.raw` is a live, initialized codec. `src`/`dst` are valid for reads/writes of
        // their stated lengths for the call. `out_len`/`out_rate`/`flag` are writable output
        // storage. A null `other_codec` is permitted.
        let status = unsafe {
            sys::switch_core_codec_encode(
                self.raw.as_ptr(),
                std::ptr::null_mut(),
                src.as_ptr().cast_mut().cast(),
                src.len() as u32,
                self.rate,
                dst.as_mut_ptr().cast(),
                &mut out_len,
                &mut out_rate,
                &mut flag,
            )
        };
        status_to_result(status)?;
        Ok(out_len as usize)
    }

    /// Decodes `src` (encoded) into `dst` (raw PCM), returning the number of bytes written.
    ///
    /// `dst` must be large enough to hold the decoded frame; on success its in-use length is written
    /// back by FreeSWITCH. A null `other_codec` is passed so only this codec's decoder runs.
    pub fn decode(&self, src: &[u8], dst: &mut [u8]) -> Result<usize> {
        let mut out_len: u32 = dst.len() as u32;
        let mut out_rate: u32 = 0;
        let mut flag: std::os::raw::c_uint = 0;
        // SAFETY: `self.raw` is a live, initialized codec. `src`/`dst` are valid for reads/writes of
        // their stated lengths for the call. `out_len`/`out_rate`/`flag` are writable output
        // storage. A null `other_codec` is permitted.
        let status = unsafe {
            sys::switch_core_codec_decode(
                self.raw.as_ptr(),
                std::ptr::null_mut(),
                src.as_ptr().cast_mut().cast(),
                src.len() as u32,
                self.rate,
                dst.as_mut_ptr().cast(),
                &mut out_len,
                &mut out_rate,
                &mut flag,
            )
        };
        status_to_result(status)?;
        Ok(out_len as usize)
    }
}

impl Drop for Codec {
    fn drop(&mut self) {
        // SAFETY: `self.raw` is a live, initialized codec owned by this wrapper. `destroy` releases
        // the codec's module-level resources; the box allocation is freed immediately after.
        let status = unsafe { sys::switch_core_codec_destroy(self.raw.as_ptr()) };
        if status != crate::SUCCESS.raw() {
            crate::log_error("codec", "codec destroy returned non-success status");
        }
        // SAFETY: `self.raw` was produced by `Box::into_raw` in `new`; it is now reclaimed.
        unsafe { drop(Box::from_raw(self.raw.as_ptr())) };
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn out_rate_is_unused_on_success_path() {
        // Smoke check that the encode/decode plumbing compiles and the output length conversion is
        // lossless for plausible frame sizes. No live codec is constructed (that needs a module
        // load + memory pool), so the FFI is exercised only at runtime.
        let bytes = 160u32;
        assert_eq!(bytes as usize, 160);
    }
}

// ── codec helpers ──────────────────────────────────────────────────────────

pub fn codec_reset(codec: *mut crate::sys::switch_codec_t) -> crate::Result<()> {
    // SAFETY: `codec` is a live codec struct per caller.
    crate::status_to_result(unsafe { crate::sys::switch_core_codec_reset(codec) })
}

pub fn codec_lock_full(session: crate::Session) {
    // SAFETY: live session.
    unsafe { crate::sys::switch_core_codec_lock_full(session.as_ptr()) };
}

pub fn codec_unlock_full(session: crate::Session) {
    // SAFETY: live session.
    unsafe { crate::sys::switch_core_codec_unlock_full(session.as_ptr()) };
}

pub fn codec_decode_video(
    codec: *mut crate::sys::switch_codec_t,
    frame: *mut crate::sys::switch_frame_t,
) -> crate::Result<()> {
    // SAFETY: live codec; valid frame ptr per caller.
    crate::status_to_result(unsafe { crate::sys::switch_core_codec_decode_video(codec, frame) })
}

pub fn codec_encode_video(
    codec: *mut crate::sys::switch_codec_t,
    frame: *mut crate::sys::switch_frame_t,
) -> crate::Result<()> {
    // SAFETY: live codec; valid frame ptr per caller.
    crate::status_to_result(unsafe { crate::sys::switch_core_codec_encode_video(codec, frame) })
}

pub fn codec_next_id() -> u32 {
    // SAFETY: no args.
    unsafe { crate::sys::switch_core_codec_next_id() }
}
