//! H.264 / VP8 video NAL packetizer — an owned wrapper over FreeSWITCH's `switch_packetizer_t`.
//!
//! FreeSWITCH's packetizer chops a framed video bitstream (H.264 Annex-B, sized H.264,
//! single-NAL, VP8, or VP9) into RTP-friendly fragments on send and is used to drive the
//! "pack one access unit into N packets" loop. This module exposes that API without
//! requiring the caller to write `unsafe`.
//!
//! The handle is owned: [`Packetizer::new`] allocates the underlying `switch_packetizer_t`
//! via `switch_packetizer_create`, and [`Drop`] hands it back to
//! `switch_packetizer_close` (which frees and nulls the handle, like the RTP / jitter
//! buffer destroy routines). The wrapper is `!Send + !Sync` because the underlying C object
//! is not thread-safe and its `feed` / `read` methods mutate C state through `&self`.
//!
//! Typical send-side usage:
//!
//! ```no_run
//! # use fswtch::packetizer::{BitstreamType, Packetizer};
//! # use fswtch::sys;
//! # fn example(pkt: &Packetizer) -> fswtch::Result<()> {
//! // 1. Feed one access unit's worth of framed NALs (borrowed, not copied — keep
//! //    `frame_data` alive until the read loop below finishes or the packetizer is fed
//! //    again).
//! let frame_data: &[u8] = &[];
//! pkt.feed(frame_data)?;
//!
//! // 2. Repeatedly read packetized frames until `read` reports done.
//! let mut frame: sys::switch_frame = Default::default();
//! let mut out = vec![0u8; 1500];
//! frame.data = out.as_mut_ptr().cast();
//! frame.buflen = out.len() as u32;
//! while pkt.read(&mut frame)? {
//!     // `frame.data` (length `frame.datalen`) is one RTP-ready packet.
//! }
//! # Ok(())
//! # }
//! ```

use std::fmt;
use std::marker::PhantomData;
use std::ptr::NonNull;

use crate::{FALSE, GENERR, Result, SUCCESS, SwitchError, status_to_result, sys};

/// The bitstream framing a [`Packetizer`] expects on feed, wrapping
/// `switch_packetizer_bitstream_t`.
///
/// Selects how `switch_packetizer_create` should interpret the bytes handed to
/// [`Packetizer::feed`]: Annex-B / sized H.264, a single H.264 NALU, or a VP8 / VP9
/// bitstream. `INVALID` is the sentinel FreeSWITCH uses to mark an unknown stream and is
/// exposed for round-tripping raw values; constructing a [`Packetizer`] with it will fail.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct BitstreamType(pub(crate) sys::switch_packetizer_bitstream_t);

impl BitstreamType {
    /// H.264 Annex-B bitstream using the `00 00 00 01` or `00 00 01` start-code separator.
    pub const H264: BitstreamType =
        BitstreamType(sys::switch_packetizer_bitstream_t_SPT_H264_BITSTREAM);

    /// H.264 sized bitstream (length-prefixed NALUs, no start codes).
    pub const H264_SIZED: BitstreamType =
        BitstreamType(sys::switch_packetizer_bitstream_t_SPT_H264_SIZED_BITSTREAM);

    /// A single H.264 NALU per feed (no separator, no length prefix).
    pub const H264_SINGLE_NALU: BitstreamType =
        BitstreamType(sys::switch_packetizer_bitstream_t_SPT_H264_SIGNALE_NALU);

    /// VP8 bitstream.
    pub const VP8: BitstreamType =
        BitstreamType(sys::switch_packetizer_bitstream_t_SPT_VP8_BITSTREAM);

    /// VP9 bitstream.
    pub const VP9: BitstreamType =
        BitstreamType(sys::switch_packetizer_bitstream_t_SPT_VP9_BITSTREAM);

    /// Sentinel marking an unrecognized stream (the trailing enum value in
    /// `switch_packetizer.h`). Constructing a [`Packetizer`] with this value will fail.
    pub const INVALID: BitstreamType =
        BitstreamType(sys::switch_packetizer_bitstream_t_SPT_INVALID_STREAM);

    /// Wraps a raw `switch_packetizer_bitstream_t` returned from FFI.
    #[inline]
    #[allow(dead_code)]
    pub(crate) const fn from_raw(raw: sys::switch_packetizer_bitstream_t) -> Self {
        Self(raw)
    }

    /// The underlying integer value, for passing back to the FFI.
    #[inline]
    pub const fn as_u32(self) -> u32 {
        self.0
    }

    /// `true` for any of the H.264 framing modes.
    #[inline]
    pub fn is_h264(self) -> bool {
        matches!(self, Self::H264 | Self::H264_SIZED | Self::H264_SINGLE_NALU)
    }

    /// `true` for the VP8 / VP9 modes.
    #[inline]
    pub fn is_vp(self) -> bool {
        matches!(self, Self::VP8 | Self::VP9)
    }

    /// `true` when this is the `INVALID` sentinel and therefore not a usable framing.
    #[inline]
    pub fn is_invalid(self) -> bool {
        self == Self::INVALID
    }
}

impl fmt::Display for BitstreamType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match *self {
            Self::H264 => "H264",
            Self::H264_SIZED => "H264_SIZED",
            Self::H264_SINGLE_NALU => "H264_SINGLE_NALU",
            Self::VP8 => "VP8",
            Self::VP9 => "VP9",
            Self::INVALID => "INVALID",
            _ => "UNKNOWN",
        };
        f.write_str(name)
    }
}

/// An owned video NAL packetizer (`switch_packetizer_t`).
///
/// Allocated with [`Packetizer::new`] and destroyed on [`Drop`] via
/// `switch_packetizer_close`. Feed a framed access unit with [`feed`](Self::feed) (and
/// optional out-of-band data with [`feed_extradata`](Self::feed_extradata)), then drain the
/// packetized fragments with [`read`](Self::read) until it returns `Ok(false)`.
///
/// The packetizer borrows fed data rather than copying it — see the safety note on
/// [`feed`](Self::feed).
pub struct Packetizer {
    raw: NonNull<sys::switch_packetizer_t>,
    // `switch_packetizer_t` is not thread-safe; `feed` / `read` mutate C state through
    // `&self`. The raw-pointer marker makes `Packetizer` `!Send + !Sync` without affecting
    // its layout.
    _marker: PhantomData<*const ()>,
}

impl Packetizer {
    /// Creates a new packetizer for `kind` bitstream framing with a `slice_size`-byte MTU.
    ///
    /// `slice_size` is the maximum payload size the packetizer will emit per read (typically
    /// the RTP MTU minus headers, e.g. `1200`). Returns
    /// [`crate::SwitchError`](`crate::GENERR`) if allocation fails or `kind` is
    /// [`BitstreamType::INVALID`].
    pub fn new(kind: BitstreamType, slice_size: u32) -> Result<Self> {
        if kind.is_invalid() {
            return Err(SwitchError(GENERR));
        }
        // SAFETY: `switch_packetizer_create` is a plain allocator taking an enum value and
        // a `uint32_t`; both arguments are plain integers. It returns NULL on failure.
        let raw = unsafe { sys::switch_packetizer_create(kind.0, slice_size) };
        NonNull::new(raw)
            .map(|raw| Self {
                raw,
                _marker: PhantomData,
            })
            .ok_or(SwitchError(GENERR))
    }

    /// Wraps a FreeSWITCH packetizer pointer created elsewhere.
    ///
    /// # Safety
    ///
    /// `raw` must point to a live `switch_packetizer_t` whose ownership the caller is
    /// transferring to this wrapper — it will be closed via `switch_packetizer_close` when
    /// the [`Packetizer`] is dropped. `raw` must not be used by any other wrapper or
    /// freed by anything else after this call.
    pub unsafe fn from_raw(raw: *mut sys::switch_packetizer_t) -> Option<Self> {
        NonNull::new(raw).map(|raw| Self {
            raw,
            _marker: PhantomData,
        })
    }

    /// The raw `switch_packetizer_t` pointer, for escape-hatch FFI.
    #[inline]
    pub fn as_ptr(&self) -> *mut sys::switch_packetizer_t {
        self.raw.as_ptr()
    }

    /// Feeds framed bitstream data into the packetizer for subsequent [`read`](Self::read)
    /// calls.
    ///
    /// Per `switch_packetizer.h`, the packetizer *borrows* `data` — "to avoid data copy,
    /// data MUST be valid before the next feed, or before close". The caller must keep the
    /// buffer live (and, in practice, unaliased) until the next `feed` call drains it or
    /// until the [`Packetizer`] is dropped.
    ///
    /// The C signature takes `void *data`, but `switch_packetizer_feed` only reads it
    /// (the borrow-only contract above means FreeSWITCH never writes back through it), so
    /// this method takes an immutable `&[u8]`. Writing an empty slice is a no-op.
    pub fn feed(&self, data: &[u8]) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        // SAFETY: `self.raw` is a live owned packetizer. `data` is a valid readable byte
        // slice for the duration of the call; FreeSWITCH borrows (does not retain or write
        // through) the pointer per the header contract, so a `*const` cast is sound.
        let status = unsafe {
            sys::switch_packetizer_feed(
                self.raw.as_ptr(),
                data.as_ptr().cast_mut().cast(),
                data.len() as u32,
            )
        };
        status_to_result(status)
    }

    /// Feeds out-of-band extra data (e.g. codec-specific header / parameter sets) into the
    /// packetizer.
    ///
    /// Carries the same borrow-only contract as [`feed`](Self::feed): keep `data` live
    /// until the next feed or until the [`Packetizer`] is dropped. Writing an empty slice
    /// is a no-op.
    pub fn feed_extradata(&self, data: &[u8]) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        // SAFETY: `self.raw` is a live owned packetizer. `data` is a valid readable byte
        // slice for the duration of the call; FreeSWITCH borrows the pointer per the header
        // contract, so a `*const` cast is sound.
        let status = unsafe {
            sys::switch_packetizer_feed_extradata(
                self.raw.as_ptr(),
                data.as_ptr().cast_mut().cast(),
                data.len() as u32,
            )
        };
        status_to_result(status)
    }

    /// Reads the next packetized frame into the caller-supplied `frame` storage.
    ///
    /// This is an escape hatch over the raw FFI: `frame` is a `*mut switch_frame_t` because
    /// the caller owns the frame's backing storage (its `data` buffer, `buflen`, etc.) and
    /// must initialize those fields before each call. See the module-level example for the
    /// expected setup.
    ///
    /// Returns `Ok(true)` when a frame was produced (examine `(*frame).data` /
    /// `(*frame).datalen` for the packet), or `Ok(false)` when the packetizer has no more
    /// fragments to emit for the data fed so far — the read loop should stop. `FALSE`
    /// (`SWITCH_STATUS_FALSE`) is FreeSWITCH's standard "nothing more" sentinel and is
    /// mapped to `Ok(false)`; any other non-success status is surfaced as
    /// [`crate::SwitchError`].
    pub fn read(&self, frame: *mut sys::switch_frame_t) -> Result<bool> {
        // SAFETY: `self.raw` is a live owned packetizer. `frame` is a valid writable
        // `switch_frame_t` supplied by the caller; `switch_packetizer_read` fills in its
        // `data` / `datalen` / `timestamp` / `seq` / `m` / `payload` fields and writes at
        // most `(*frame).buflen` bytes through `(*frame).data`.
        let status = unsafe { sys::switch_packetizer_read(self.raw.as_ptr(), frame) };
        if status == SUCCESS.raw() {
            Ok(true)
        } else if status == FALSE.raw() {
            Ok(false)
        } else {
            Err(SwitchError(crate::Status::from_raw(status)))
        }
    }
}

impl Drop for Packetizer {
    fn drop(&mut self) {
        let mut ptr = self.raw.as_ptr();
        // SAFETY: `self.raw` owns exactly one `switch_packetizer_t`. `switch_packetizer_close`
        // takes the handle by `*mut *mut` so it can free and null it; the handle is not shared,
        // so a single close is correct. After this returns the pointer is invalid and `ptr` is
        // expected to have been nulled.
        unsafe { sys::switch_packetizer_close(&mut ptr) };
        debug_assert!(
            ptr.is_null(),
            "switch_packetizer_close must null the handle on success"
        );
    }
}

impl fmt::Debug for Packetizer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Packetizer")
            .field("ptr", &self.raw)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitstream_type_consts_are_distinct_and_named() {
        assert_ne!(BitstreamType::H264, BitstreamType::VP8);
        assert_ne!(BitstreamType::H264, BitstreamType::H264_SIZED);
        assert_ne!(BitstreamType::H264, BitstreamType::H264_SINGLE_NALU);
        assert_ne!(BitstreamType::VP8, BitstreamType::VP9);
        assert!(BitstreamType::INVALID.is_invalid());
        assert!(BitstreamType::H264.is_h264());
        assert!(BitstreamType::H264_SIZED.is_h264());
        assert!(BitstreamType::H264_SINGLE_NALU.is_h264());
        assert!(BitstreamType::VP8.is_vp());
        assert!(BitstreamType::VP9.is_vp());
        assert!(!BitstreamType::H264.is_vp());
        assert!(!BitstreamType::VP8.is_h264());
    }

    #[test]
    fn bitstream_type_round_trips() {
        for raw in [
            sys::switch_packetizer_bitstream_t_SPT_H264_BITSTREAM,
            sys::switch_packetizer_bitstream_t_SPT_VP8_BITSTREAM,
            sys::switch_packetizer_bitstream_t_SPT_INVALID_STREAM,
        ] {
            assert_eq!(BitstreamType::from_raw(raw).as_u32(), raw);
        }
    }

    #[test]
    fn bitstream_type_display_covers_known_values() {
        assert_eq!(BitstreamType::H264.to_string(), "H264");
        assert_eq!(BitstreamType::VP8.to_string(), "VP8");
        assert_eq!(BitstreamType::INVALID.to_string(), "INVALID");
    }
}
