//! FIFO byte buffer wrapping FreeSWITCH's `switch_buffer_t`.
//!
//! A [`Buffer`] is a growable, dynamically allocated FIFO of bytes used throughout FreeSWITCH for
//! media framing, jitter buffering, and similar streaming tasks. This wrapper owns the underlying
//! `switch_buffer_t`: it is created with `switch_buffer_create_dynamic` (no memory pool required)
//! and destroyed with `switch_buffer_destroy` on [`Drop`].
//!
//! The public API exposes only `&[u8]` / `&mut [u8]` slices and `usize` counts — no raw pointers,
//! no `unsafe` from the caller.

use std::marker::PhantomData;
use std::ptr::NonNull;

use crate::{GENERR, Result, SwitchError, status_to_result, sys};

/// The realloc granularity applied when a dynamic buffer needs to grow, in bytes.
///
/// A power-of-two multiple of the typical frame size keeps fragmentation and copy overhead low.
const DEFAULT_BLOCKSIZE: usize = 1024;

/// An owned, dynamically allocated FIFO byte buffer (`switch_buffer_t`).
///
/// Created with [`Buffer::new`] / [`Buffer::with_growth`]. The buffer grows on demand up to its
/// configured `max_len` and is freed automatically when dropped.
///
/// `write` appends to the tail; `read` / `peek` consume from / observe the head. `toss` discards
/// bytes from the head without copying them out. `inuse` reports bytes currently buffered, while
/// `len` reports the buffer's total capacity and `freespace` the bytes still writable before the
/// `max_len` ceiling is reached.
pub struct Buffer {
    raw: NonNull<sys::switch_buffer_t>,
    // `switch_buffer_t` is not thread-safe and `&self` methods mutate C state, so `Buffer` is
    // neither `Send` nor `Sync`. The raw-pointer marker enforces this without affecting layout.
    _marker: PhantomData<*const ()>,
}

impl Buffer {
    /// Creates a new dynamic buffer that can hold up to `capacity` bytes.
    ///
    /// The buffer starts with a small reservation and grows in [`DEFAULT_BLOCKSIZE`]-byte
    /// increments as data is written, up to `capacity`. For finer control over the growth
    /// parameters use [`Buffer::with_growth`].
    pub fn new(capacity: usize) -> Result<Self> {
        Self::with_growth(DEFAULT_BLOCKSIZE, DEFAULT_BLOCKSIZE, capacity)
    }

    /// Creates a new dynamic buffer with explicit growth parameters.
    ///
    /// `blocksize` is the realloc granularity as data is added, `start_len` is the memory reserved
    /// initially, and `max_len` is the maximum size the buffer is allowed to grow to. Passing `0`
    /// for `max_len` lets the buffer grow without bound (per FreeSWITCH's contract).
    pub fn with_growth(blocksize: usize, start_len: usize, max_len: usize) -> Result<Self> {
        let mut raw: *mut sys::switch_buffer_t = std::ptr::null_mut();
        // SAFETY: `raw` is a valid out-pointer; the size arguments are plain integers. On success
        // `*raw` is a heap-allocated, owned `switch_buffer_t`.
        let status = unsafe {
            sys::switch_buffer_create_dynamic(
                &mut raw as *mut *mut sys::switch_buffer_t,
                blocksize as sys::switch_size_t,
                start_len as sys::switch_size_t,
                max_len as sys::switch_size_t,
            )
        };
        status_to_result(status)?;
        // SAFETY: `switch_buffer_create_dynamic` returned SUCCESS, so `raw` is a valid non-null
        // owned handle.
        let raw = NonNull::new(raw).ok_or(SwitchError(GENERR))?;
        Ok(Self {
            raw,
            _marker: PhantomData,
        })
    }

    /// Wraps a FreeSWITCH buffer pointer created elsewhere.
    ///
    /// # Safety
    ///
    /// `raw` must point to a live `switch_buffer_t` that the caller is willing to hand over for
    /// destruction via `switch_buffer_destroy` when this `Buffer` is dropped. Wrapping a
    /// pool-allocated buffer is unsound — its storage belongs to the pool, not this wrapper.
    pub unsafe fn from_raw(raw: *mut sys::switch_buffer_t) -> Option<Self> {
        NonNull::new(raw).map(|raw| Self {
            raw,
            _marker: PhantomData,
        })
    }

    /// Returns the raw `switch_buffer_t` pointer for escape-hatch FFI.
    #[inline]
    pub fn as_ptr(&self) -> *mut sys::switch_buffer_t {
        self.raw.as_ptr()
    }

    /// Appends `data` to the tail of the buffer.
    ///
    /// Returns an error when the buffer has no free space left (the underlying `switch_buffer_write`
    /// reports the post-write in-use count and yields `0` to signal that nothing could be written).
    /// Writing an empty slice is a no-op and always succeeds.
    pub fn write(&self, data: &[u8]) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        // SAFETY: `self.raw` is a live owned buffer; `data` is a valid byte slice for the duration
        // of the call.
        let written = unsafe {
            sys::switch_buffer_write(
                self.raw.as_ptr(),
                data.as_ptr() as *const ::std::os::raw::c_void,
                data.len() as sys::switch_size_t,
            )
        };
        if written == 0 {
            Err(SwitchError(GENERR))
        } else {
            Ok(())
        }
    }

    /// Reads up to `dst.len()` bytes from the head of the buffer, removing them. Returns the
    /// number of bytes actually read (which may be less than requested when the buffer is short).
    pub fn read(&self, dst: &mut [u8]) -> usize {
        if dst.is_empty() {
            return 0;
        }
        // SAFETY: `self.raw` is a live owned buffer; `dst` is a valid mutable byte slice for the
        // duration of the call. `switch_buffer_read` writes at most `dst.len()` bytes.
        let n = unsafe {
            sys::switch_buffer_read(
                self.raw.as_ptr(),
                dst.as_mut_ptr() as *mut ::std::os::raw::c_void,
                dst.len() as sys::switch_size_t,
            )
        };
        n as usize
    }

    /// Copies up to `dst.len()` bytes from the head of the buffer without removing them. Returns
    /// the number of bytes actually copied.
    pub fn peek(&self, dst: &mut [u8]) -> usize {
        if dst.is_empty() {
            return 0;
        }
        // SAFETY: `self.raw` is a live owned buffer; `dst` is a valid mutable byte slice for the
        // duration of the call. `switch_buffer_peek` writes at most `dst.len()` bytes and does not
        // advance the read position.
        let n = unsafe {
            sys::switch_buffer_peek(
                self.raw.as_ptr(),
                dst.as_mut_ptr() as *mut ::std::os::raw::c_void,
                dst.len() as sys::switch_size_t,
            )
        };
        n as usize
    }

    /// Discards up to `n` bytes from the head of the buffer. No error is raised when fewer bytes
    /// are available — the buffer is simply emptied.
    pub fn toss(&self, n: usize) {
        if n == 0 {
            return;
        }
        // SAFETY: `self.raw` is a live owned buffer; `n` is a plain count.
        unsafe { sys::switch_buffer_toss(self.raw.as_ptr(), n as sys::switch_size_t) };
    }

    /// Writes `len` zero bytes into the buffer (via `switch_buffer_zwrite`). Returns an error when
    /// the buffer cannot accept the request.
    ///
    /// `switch_buffer_zwrite`'s `data` parameter is SAL-annotated `_In_bytecount_(datalen)`, so it
    /// requires a valid pointer even though the bytes are zero — a null pointer with a non-zero
    /// length violates the contract. This method writes from a small stack-allocated zero buffer
    /// in chunks to stay within the documented ABI.
    pub fn zero_fill(&self, len: usize) -> Result<()> {
        if len == 0 {
            return Ok(());
        }
        const CHUNK: usize = 256;
        let zeros = [0u8; CHUNK];
        let mut remaining = len;
        while remaining > 0 {
            let n = remaining.min(CHUNK) as sys::switch_size_t;
            // SAFETY: `self.raw` is a live owned buffer; `zeros` is a valid readable buffer of `n`
            // bytes (n <= CHUNK).
            let written =
                unsafe { sys::switch_buffer_zwrite(self.raw.as_ptr(), zeros.as_ptr().cast(), n) };
            if written == 0 {
                return Err(SwitchError(GENERR));
            }
            remaining -= written as usize;
        }
        Ok(())
    }

    /// Returns the number of bytes that can still be written before the buffer reaches its
    /// `max_len` ceiling.
    pub fn freespace(&self) -> usize {
        // SAFETY: `self.raw` is a live owned buffer.
        unsafe { sys::switch_buffer_freespace(self.raw.as_ptr()) as usize }
    }

    /// Returns the number of bytes currently buffered (not yet read out).
    pub fn inuse(&self) -> usize {
        // SAFETY: `self.raw` is a live owned buffer.
        unsafe { sys::switch_buffer_inuse(self.raw.as_ptr()) as usize }
    }

    /// Returns the buffer's total capacity (its configured `max_len`).
    pub fn len(&self) -> usize {
        // SAFETY: `self.raw` is a live owned buffer.
        unsafe { sys::switch_buffer_len(self.raw.as_ptr()) as usize }
    }

    /// Returns `true` when no bytes are currently buffered.
    pub fn is_empty(&self) -> bool {
        self.inuse() == 0
    }

    /// Removes all buffered data without freeing the underlying storage.
    pub fn zero(&self) {
        // SAFETY: `self.raw` is a live owned buffer.
        unsafe { sys::switch_buffer_zero(self.raw.as_ptr()) };
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        let mut raw = self.raw.as_ptr();
        // SAFETY: `raw` is the owned, non-null buffer handle created by `switch_buffer_create_dynamic`.
        // `switch_buffer_destroy` frees the buffer and nulls `*raw`; per the header it is "only
        // necessary on dynamic buffers", which is exactly how this wrapper allocates. The handle
        // is not shared — this `Buffer` owns it exclusively — so a single destroy is correct.
        unsafe { sys::switch_buffer_destroy(&mut raw) };
        debug_assert!(
            raw.is_null(),
            "switch_buffer_destroy must null the handle on success"
        );
    }
}

impl std::fmt::Debug for Buffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Buffer")
            .field("inuse", &self.inuse())
            .field("len", &self.len())
            .field("freespace", &self.freespace())
            .finish()
    }
}

#[cfg(all(test, feature = "live_fs"))]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_write_read() {
        let buf = Buffer::new(1024).expect("create buffer");
        assert!(buf.is_empty());
        buf.write(b"hello world").expect("write");
        assert_eq!(buf.inuse(), 11);

        let mut out = [0u8; 5];
        let n = buf.read(&mut out);
        assert_eq!(n, 5);
        assert_eq!(&out, b"hello");
        assert_eq!(buf.inuse(), 6);
    }

    #[test]
    fn peek_does_not_consume() {
        let buf = Buffer::new(1024).expect("create buffer");
        buf.write(b"abcdef").expect("write");

        let mut out = [0u8; 3];
        let n = buf.peek(&mut out);
        assert_eq!(n, 3);
        assert_eq!(&out, b"abc");
        assert_eq!(buf.inuse(), 6, "peek must not remove data");
    }

    #[test]
    fn toss_discards_head() {
        let buf = Buffer::new(1024).expect("create buffer");
        buf.write(b"abcdef").expect("write");
        buf.toss(2);
        assert_eq!(buf.inuse(), 4);

        let mut out = [0u8; 4];
        assert_eq!(buf.read(&mut out), 4);
        assert_eq!(&out, b"cdef");
    }

    #[test]
    fn zero_clears_buffer() {
        let buf = Buffer::new(1024).expect("create buffer");
        buf.write(b"data").expect("write");
        buf.zero();
        assert!(buf.is_empty());
    }

    #[test]
    fn zero_fill_writes_zeros() {
        let buf = Buffer::new(1024).expect("create buffer");
        buf.zero_fill(4).expect("zero fill");
        assert_eq!(buf.inuse(), 4);

        let mut out = [0xffu8; 4];
        buf.read(&mut out);
        assert_eq!(out, [0u8; 4]);
    }

    #[test]
    fn read_returns_partial_when_short() {
        let buf = Buffer::new(1024).expect("create buffer");
        buf.write(b"abc").expect("write");

        let mut out = [0u8; 10];
        let n = buf.read(&mut out);
        assert_eq!(n, 3);
        assert!(buf.is_empty());
    }
}
