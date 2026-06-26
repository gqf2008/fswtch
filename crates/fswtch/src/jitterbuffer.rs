//! FreeSWITCH jitter buffer (`switch_jb_t`).
//!
//! A jitter buffer smooths out network timing variation for RTP media by reordering packets,
//! dropping late arrivals, and emitting a steady frame stream. This module wraps the
//! `switch_jb_*` family so a safe-Rust caller can put/get packets, poll for readiness, and
//! introspect buffer depth without touching raw pointers.
//!
//! The buffer owns its own memory pool (created via `switch_core_perform_new_memory_pool` and
//! torn down alongside the buffer in [`JitterBuffer::drop`]), so constructing a
//! [`JitterBuffer`] requires no external pool handle.

use std::marker::PhantomData;
use std::ptr::NonNull;

use crate::{GENERR, Result, SwitchError, status_to_result, sys};

/// The media kind a [`JitterBuffer`] carries.
///
/// Mirrors `switch_jb_type_t` (`SJB_VIDEO`, `SJB_AUDIO`, `SJB_TEXT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JbKind {
    /// Video packets (`SJB_VIDEO`).
    Video,
    /// Audio packets (`SJB_AUDIO`).
    Audio,
    /// Text packets (`SJB_TEXT`).
    Text,
}

impl JbKind {
    #[inline]
    fn as_raw(self) -> sys::switch_jb_type_t {
        match self {
            JbKind::Video => sys::switch_jb_type_t_SJB_VIDEO,
            JbKind::Audio => sys::switch_jb_type_t_SJB_AUDIO,
            JbKind::Text => sys::switch_jb_type_t_SJB_TEXT,
        }
    }
}

/// Optional behaviour flag for a [`JitterBuffer`], mirroring `switch_jb_flag_t`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JbFlag {
    /// `SJB_QUEUE_ONLY` — accept packets into the queue without emitting them on read.
    QueueOnly,
}

impl JbFlag {
    #[inline]
    fn as_raw(self) -> sys::switch_jb_flag_t {
        match self {
            JbFlag::QueueOnly => sys::switch_jb_flag_t_SJB_QUEUE_ONLY,
        }
    }
}

/// Build configuration for a [`JitterBuffer`].
///
/// FreeSWITCH's `switch_jb_create` needs the media type plus minimum and maximum frame
/// lengths (in milliseconds). This builder captures those before construction; sensible
/// audio defaults are applied when fields are left unset.
///
/// ```
/// use fswtch::{JbKind, JitterBuffer, JitterBufferConfig};
///
/// # fn main() -> fswtch::Result<()> {
/// let jb = JitterBuffer::new(
///     JitterBufferConfig::new(JbKind::Audio)
///         .min_frame_len(20)
///         .max_frame_len(120),
/// )?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Copy)]
pub struct JitterBufferConfig {
    kind: JbKind,
    min_frame_len: u32,
    max_frame_len: u32,
}

impl JitterBufferConfig {
    /// Begins a config for the given media `kind` with audio-style frame-length defaults
    /// (`min = 20 ms`, `max = 120 ms`). Use [`Self::min_frame_len`] / [`Self::max_frame_len`]
    /// to override.
    pub const fn new(kind: JbKind) -> Self {
        Self {
            kind,
            min_frame_len: 20,
            max_frame_len: 120,
        }
    }

    /// Sets the minimum frame length, in milliseconds.
    pub const fn min_frame_len(mut self, ms: u32) -> Self {
        self.min_frame_len = ms;
        self
    }

    /// Sets the maximum frame length, in milliseconds.
    pub const fn max_frame_len(mut self, ms: u32) -> Self {
        self.max_frame_len = ms;
        self
    }
}

/// A snapshot of the jitter buffer's frame-length state, as returned by
/// [`JitterBuffer::frames`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JbFrames {
    /// Configured minimum frame length (ms).
    pub min_frame_len: u32,
    /// Configured maximum frame length (ms).
    pub max_frame_len: u32,
    /// Current running frame length (ms).
    pub cur_frame_len: u32,
    /// Highest frame length observed so far (ms).
    pub highest_frame_len: u32,
}

/// An owned handle to a FreeSWITCH jitter buffer (`switch_jb_t`).
///
/// Construct with [`JitterBuffer::new`]; the handle owns its backing memory pool and the
/// buffer itself, both freed on drop. The methods wrap `switch_jb_put_packet`,
/// `switch_jb_get_packet`, `switch_jb_poll`, and the introspection helpers.
pub struct JitterBuffer {
    raw: NonNull<sys::switch_jb_t>,
    // Owned memory pool passed to `switch_jb_create`. Held as an `Option` so that `Drop` can
    // take ownership of the pointer while keeping the field non-`None` until then.
    pool: Option<NonNull<sys::switch_memory_pool_t>>,
    // Not thread-safe; `put_packet`/`get_packet` mutate C state through `&self`.
    _marker: PhantomData<*const ()>,
}

impl JitterBuffer {
    /// Creates a new jitter buffer from `config`.
    ///
    /// A private FreeSWITCH memory pool is allocated for the buffer (via
    /// `switch_core_perform_new_memory_pool`); the pool and the buffer are released together
    /// when this handle is dropped.
    pub fn new(config: JitterBufferConfig) -> Result<Self> {
        let mut pool: *mut sys::switch_memory_pool_t = std::ptr::null_mut();
        // SAFETY: `pool` is a fresh, null out-pointer; the file/func/line strings are static
        // C strings used only for logging. On success `*pool` is a live allocator handle.
        let status = unsafe {
            sys::switch_core_perform_new_memory_pool(
                &mut pool,
                c"fswtch-rs".as_ptr(),
                c"JitterBuffer::new".as_ptr(),
                line!() as _,
            )
        };
        status_to_result(status)?;
        let pool = NonNull::new(pool).ok_or(SwitchError(GENERR))?;

        let mut jb: *mut sys::switch_jb_t = std::ptr::null_mut();
        // SAFETY: `jb` is a fresh out-pointer, `pool` is the live pool just created, and the
        // frame lengths come from a validated `config`.
        let status = unsafe {
            sys::switch_jb_create(
                &mut jb,
                config.kind.as_raw(),
                config.min_frame_len,
                config.max_frame_len,
                pool.as_ptr(),
            )
        };
        if status_to_result(status).is_err() {
            // SAFETY: `pool` is still live and owned by us; destroy it before propagating the
            // error so we never leak it.
            let mut p = pool.as_ptr();
            unsafe {
                sys::switch_core_perform_destroy_memory_pool(
                    &mut p,
                    c"fswtch-rs".as_ptr(),
                    c"JitterBuffer::new".as_ptr(),
                    line!() as _,
                )
            };
            return Err(SwitchError(status));
        }
        let raw = NonNull::new(jb).ok_or(SwitchError(GENERR))?;

        Ok(Self {
            raw,
            pool: Some(pool),
            _marker: PhantomData,
        })
    }

    /// Returns the raw `switch_jb_t` pointer for escape-hatch FFI.
    ///
    /// The pointer is valid for the lifetime of this [`JitterBuffer`].
    #[inline]
    pub fn as_ptr(&self) -> *mut sys::switch_jb_t {
        self.raw.as_ptr()
    }

    /// Queues a complete RTP packet (header + payload) into the buffer.
    ///
    /// `len` is the *total* number of meaningful bytes in `packet` (header plus payload),
    /// which FreeSWITCH uses to size internal copies. The packet struct is borrowed for the
    /// call only; it is copied into the buffer.
    ///
    /// Wraps `switch_jb_put_packet`.
    pub fn put_packet(&self, packet: &sys::switch_rtp_packet_t, len: usize) -> Result<()> {
        // SAFETY: `self.raw` is a live buffer; `packet` is a shared reference to a valid
        // struct for the duration of the call (read-only access from C).
        let status = unsafe {
            sys::switch_jb_put_packet(
                self.raw.as_ptr(),
                packet as *const sys::switch_rtp_packet_t as *mut sys::switch_rtp_packet_t,
                len,
            )
        };
        status_to_result(status)
    }

    /// Attempts to retrieve the next ready packet from the buffer.
    ///
    /// `packet` receives the decoded packet on success and `len` is an in/out parameter: on
    /// entry it is the capacity of `packet`'s body, on success it is the number of bytes
    /// written. Returns `Ok(true)` when a packet was returned, `Ok(false)` when the buffer
    /// had nothing ready.
    ///
    /// Wraps `switch_jb_get_packet`.
    pub fn get_packet(
        &self,
        packet: &mut sys::switch_rtp_packet_t,
        len: &mut usize,
    ) -> Result<bool> {
        // SAFETY: `self.raw` is a live buffer; `packet` is a valid exclusive reference whose
        // storage C may write into; `len` points to a live `usize` used in/out.
        let status = unsafe {
            sys::switch_jb_get_packet(
                self.raw.as_ptr(),
                packet as *mut sys::switch_rtp_packet_t,
                len as *mut usize as *mut sys::switch_size_t,
            )
        };
        match status_to_result(status) {
            Ok(()) => Ok(true),
            Err(e) if e.0 == crate::FALSE => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Retrieves the packet matching sequence number `seq`, if present.
    ///
    /// `packet` and `len` behave as in [`JitterBuffer::get_packet`]. Returns `Ok(true)` on a
    /// hit, `Ok(false)` when no packet with that sequence number is queued.
    ///
    /// Wraps `switch_jb_get_packet_by_seq`.
    pub fn get_packet_by_seq(
        &self,
        seq: u16,
        packet: &mut sys::switch_rtp_packet_t,
        len: &mut usize,
    ) -> Result<bool> {
        // SAFETY: `self.raw` is live; `packet`/`len` are valid exclusive references for the call.
        let status = unsafe {
            sys::switch_jb_get_packet_by_seq(
                self.raw.as_ptr(),
                seq,
                packet as *mut sys::switch_rtp_packet_t,
                len as *mut usize as *mut sys::switch_size_t,
            )
        };
        match status_to_result(status) {
            Ok(()) => Ok(true),
            Err(e) if e.0 == crate::FALSE => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Inspects a queued frame without removing it.
    ///
    /// Looks up the frame near timestamp `ts` / sequence `seq`; `peek` controls how many
    /// frames ahead to look. On success `frame` is filled with a view of the queued frame.
    ///
    /// Wraps `switch_jb_peek_frame`.
    pub fn peek_frame(
        &self,
        ts: u32,
        seq: u16,
        peek: i32,
        frame: &mut sys::switch_frame_t,
    ) -> Result<bool> {
        // SAFETY: `self.raw` is live; `frame` is a valid exclusive reference for the call.
        let status = unsafe {
            sys::switch_jb_peek_frame(
                self.raw.as_ptr(),
                ts,
                seq,
                peek,
                frame as *mut sys::switch_frame_t,
            )
        };
        match status_to_result(status) {
            Ok(()) => Ok(true),
            Err(e) if e.0 == crate::FALSE => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Polls the buffer for readiness.
    ///
    /// Returns the number of frames now available to read (a non-negative count). Zero means
    /// nothing is ready. Wraps `switch_jb_poll`.
    pub fn poll(&self) -> i32 {
        // SAFETY: `self.raw` is a live buffer.
        unsafe { sys::switch_jb_poll(self.raw.as_ptr()) }
    }

    /// The number of frames currently held in the buffer.
    ///
    /// Wraps `switch_jb_frame_count`.
    pub fn frame_count(&self) -> i32 {
        // SAFETY: `self.raw` is a live buffer.
        unsafe { sys::switch_jb_frame_count(self.raw.as_ptr()) }
    }

    /// Reports the configured and observed frame lengths.
    ///
    /// Wraps `switch_jb_get_frames`. Pointers that are not needed may pass `None`.
    pub fn frames(&self) -> Result<JbFrames> {
        let mut min_frame_len: u32 = 0;
        let mut max_frame_len: u32 = 0;
        let mut cur_frame_len: u32 = 0;
        let mut highest_frame_len: u32 = 0;
        // SAFETY: `self.raw` is live; all four out-pointers reference valid `u32` storage.
        let status = unsafe {
            sys::switch_jb_get_frames(
                self.raw.as_ptr(),
                &mut min_frame_len,
                &mut max_frame_len,
                &mut cur_frame_len,
                &mut highest_frame_len,
            )
        };
        status_to_result(status)?;
        Ok(JbFrames {
            min_frame_len,
            max_frame_len,
            cur_frame_len,
            highest_frame_len,
        })
    }

    /// Reconfigures the minimum and maximum frame lengths.
    ///
    /// Wraps `switch_jb_set_frames`.
    pub fn set_frames(&self, min_frame_len: u32, max_frame_len: u32) -> Result<()> {
        // SAFETY: `self.raw` is a live buffer.
        let status =
            unsafe { sys::switch_jb_set_frames(self.raw.as_ptr(), min_frame_len, max_frame_len) };
        status_to_result(status)
    }

    /// The number of bytes read from the last [`JitterBuffer::get_packet`] call.
    ///
    /// Wraps `switch_jb_get_last_read_len`.
    pub fn last_read_len(&self) -> usize {
        // SAFETY: `self.raw` is a live buffer.
        unsafe { sys::switch_jb_get_last_read_len(self.raw.as_ptr()) }
    }

    /// Pops and returns the latest NACK (negative acknowledgement) count, if any.
    ///
    /// Wraps `switch_jb_pop_nack`.
    pub fn pop_nack(&self) -> u32 {
        // SAFETY: `self.raw` is a live buffer.
        unsafe { sys::switch_jb_pop_nack(self.raw.as_ptr()) }
    }

    /// The count of successful NACK operations so far.
    ///
    /// Wraps `switch_jb_get_nack_success`.
    pub fn nack_success(&self) -> u32 {
        // SAFETY: `self.raw` is a live buffer.
        unsafe { sys::switch_jb_get_nack_success(self.raw.as_ptr()) }
    }

    /// The number of packets comprising a single frame.
    ///
    /// Wraps `switch_jb_get_packets_per_frame`.
    pub fn packets_per_frame(&self) -> u32 {
        // SAFETY: `self.raw` is a live buffer.
        unsafe { sys::switch_jb_get_packets_per_frame(self.raw.as_ptr()) }
    }

    /// Configures timestamp interpretation for this buffer.
    ///
    /// Wraps `switch_jb_ts_mode`.
    pub fn set_ts_mode(&self, samples_per_frame: u32, samples_per_second: u32) {
        // SAFETY: `self.raw` is a live buffer.
        unsafe { sys::switch_jb_ts_mode(self.raw.as_ptr(), samples_per_frame, samples_per_second) };
    }

    /// Attaches a jitter estimator, writing the current jitter into `jitter`.
    ///
    /// Wraps `switch_jb_set_jitter_estimator`.
    pub fn set_jitter_estimator(
        &self,
        jitter: &mut f64,
        samples_per_frame: u32,
        samples_per_second: u32,
    ) {
        // SAFETY: `self.raw` is live; `jitter` is a valid exclusive `f64` reference.
        unsafe {
            sys::switch_jb_set_jitter_estimator(
                self.raw.as_ptr(),
                jitter as *mut f64,
                samples_per_frame,
                samples_per_second,
            )
        };
    }

    /// Associates the buffer with a session (optional, for logging/state).
    ///
    /// # Safety
    ///
    /// `session` must be a live `switch_core_session_t` pointer (or null to detach) that
    /// remains valid for as long as the buffer references it.
    pub unsafe fn set_session(&self, session: *mut sys::switch_core_session_t) {
        // SAFETY: forwarded to the caller's `# Safety` contract.
        unsafe { sys::switch_jb_set_session(self.raw.as_ptr(), session) };
    }

    /// Sets a behaviour flag on the buffer.
    ///
    /// Wraps `switch_jb_set_flag`.
    pub fn set_flag(&self, flag: JbFlag) {
        // SAFETY: `self.raw` is a live buffer.
        unsafe { sys::switch_jb_set_flag(self.raw.as_ptr(), flag.as_raw()) };
    }

    /// Clears a behaviour flag on the buffer.
    ///
    /// Wraps `switch_jb_clear_flag`.
    pub fn clear_flag(&self, flag: JbFlag) {
        // SAFETY: `self.raw` is a live buffer.
        unsafe { sys::switch_jb_clear_flag(self.raw.as_ptr(), flag.as_raw()) };
    }

    /// Empties the buffer without destroying it.
    ///
    /// Wraps `switch_jb_reset`.
    pub fn reset(&self) {
        // SAFETY: `self.raw` is a live buffer.
        unsafe { sys::switch_jb_reset(self.raw.as_ptr()) };
    }

    /// Sets the debug verbosity (0 = off).
    ///
    /// Wraps `switch_jb_debug_level`.
    pub fn set_debug_level(&self, level: u8) {
        // SAFETY: `self.raw` is a live buffer.
        unsafe { sys::switch_jb_debug_level(self.raw.as_ptr(), level) };
    }
}

impl Drop for JitterBuffer {
    fn drop(&mut self) {
        // Destroy the buffer first, then its backing pool. Both take `*mut *mut` so we hand
        // them a live pointer slot and they null it on success.
        let mut jb = self.raw.as_ptr();
        // SAFETY: `self.raw` is a live buffer owned solely by this handle; the pool was passed
        // to `switch_jb_create` and is freed below after the buffer is gone.
        unsafe {
            sys::switch_jb_destroy(&mut jb);
        }
        if let Some(pool) = self.pool.take() {
            let mut p = pool.as_ptr();
            // SAFETY: `pool` is the live, owned pool created in `new` and not yet destroyed.
            unsafe {
                sys::switch_core_perform_destroy_memory_pool(
                    &mut p,
                    c"fswtch-rs".as_ptr(),
                    c"JitterBuffer::drop".as_ptr(),
                    line!() as _,
                )
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_are_audio_20_120() {
        let cfg = JitterBufferConfig::new(JbKind::Audio);
        assert_eq!(cfg.min_frame_len, 20);
        assert_eq!(cfg.max_frame_len, 120);
    }

    #[test]
    fn config_overrides_apply() {
        let cfg = JitterBufferConfig::new(JbKind::Video)
            .min_frame_len(10)
            .max_frame_len(60);
        assert_eq!(cfg.min_frame_len, 10);
        assert_eq!(cfg.max_frame_len, 60);
        assert_eq!(cfg.kind, JbKind::Video);
    }

    #[test]
    fn kind_maps_to_raw() {
        assert_eq!(JbKind::Video.as_raw(), sys::switch_jb_type_t_SJB_VIDEO);
        assert_eq!(JbKind::Audio.as_raw(), sys::switch_jb_type_t_SJB_AUDIO);
        assert_eq!(JbKind::Text.as_raw(), sys::switch_jb_type_t_SJB_TEXT);
    }

    #[test]
    fn flag_maps_to_raw() {
        assert_eq!(
            JbFlag::QueueOnly.as_raw(),
            sys::switch_jb_flag_t_SJB_QUEUE_ONLY
        );
    }
}
