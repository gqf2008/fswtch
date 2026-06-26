//! Safe wrappers for FreeSWITCH endpoint I/O routines.
//!
//! A FreeSWITCH endpoint interface (`switch_endpoint_interface`) owns a
//! `switch_io_routines` struct: a table of 14 optional `extern "C" fn` callbacks
//! (plus padding) that FreeSWITCH invokes to drive a session's media, signalling,
//! and state. The module-registration layer in [`crate::module`] takes a raw
//! `*mut sys::switch_io_routines_t`; this module gives module authors a safe
//! [`IoRoutinesBuilder`] to construct that pointer without touching `unsafe`,
//! plus safe argument wrappers ([`Dtmf`], [`IoFlags`], [`Frame`], [`FrameMut`],
//! [`SessionMessage`]) for the bodies of those callbacks.
//!
//! The I/O callbacks receive **no `user_data` parameter** — unlike media bugs,
//! there is no closure slot threaded through the table. The native pattern is
//! for the module to supply plain `extern "C" fn` trampolines and recover
//! per-interface state from the session's `endpoint_interface->private_info`.
//! Because that recovery path is not cleanly exposed by the bindgen bindings,
//! this module deliberately stops at the builder + argument-wrapper layer; a
//! full trait-object ergonomic layer is deferred (see `DEFERRED` below).
//!
//! # Example
//!
//! ```no_run
//! use fswtch::{IoRoutinesBuilder, ModuleBuilder};
//! # use fswtch::sys;
//! # fn build(module: ModuleBuilder) -> fswtch::Result<()> {
//! let io = IoRoutinesBuilder::new()
//!     .kill_channel(Some(kill_trampoline))
//!     .state_change(Some(state_change_trampoline))
//!     .build()?;
//! let module = module.endpoint("my_endpoint", io)?.finish();
//! # Ok(())
//! # }
//! # unsafe extern "C" fn kill_trampoline(
//! #     _: *mut sys::switch_core_session_t,
//! #     _: std::os::raw::c_int,
//! # ) -> fswtch::Status { fswtch::SUCCESS }
//! # unsafe extern "C" fn state_change_trampoline(
//! #     _: *mut sys::switch_core_session_t,
//! # ) -> fswtch::Status { fswtch::SUCCESS }
//! ```
//!
//! [`crate::module`]: crate::module

use std::ffi::c_void;
use std::os::raw::c_char;
use std::ptr::NonNull;

use crate::{MediaFrame, MediaFrameMut, Result, sys};
use crate::command::borrowed_cstr_to_str;

/// A borrowed view of a FreeSWITCH media frame in an endpoint I/O callback.
///
/// This is a re-export of [`crate::MediaFrame`]: the underlying
/// `switch_frame_t` wrapper is not specific to media bugs and is reused here
/// so the `read_frame` / `write_frame` trampolines can share one frame type.
pub type Frame<'a> = MediaFrame<'a>;

/// A mutable borrowed view of a FreeSWITCH media frame in an endpoint I/O callback.
///
/// This is a re-export of [`crate::MediaFrameMut`].
pub type FrameMut<'a> = MediaFrameMut<'a>;

/// A DTMF event, mirroring `switch_dtmf_t`.
///
/// `switch_dtmf_t` is a plain `#[repr(C)]` struct, so this newtype wraps it
/// transparently and exposes safe accessors plus a [`Dtmf::new`] constructor
/// for building DTMF to send into the `send_dtmf` callback. The inner
/// `switch_dtmf_t` is reachable via [`Dtmf::into_inner`] / [`Dtmf::as_ptr`]
/// when interop with the C ABI is required.
#[repr(transparent)]
#[derive(Debug, Copy, Clone)]
pub struct Dtmf(sys::switch_dtmf_t);

impl Dtmf {
    /// Creates a DTMF event with the given digit (an ASCII character such as
    /// `'1'` or `'#'`), duration in samples, and source. `flags` defaults to
    /// zero when constructed via [`Dtmf::new`]; use [`Dtmf::with_flags`] to set it.
    ///
    /// Returns [`crate::SwitchError`](`crate::GENERR`) when `digit` is not a single ASCII byte
    /// (the C `switch_dtmf_t.digit` field is one `c_char`, so non-ASCII input would be silently
    /// truncated).
    pub fn new(digit: char, duration: u32, source: DtmfSource) -> Result<Self> {
        let digit = c_char::try_from(digit as u32).map_err(|_| crate::SwitchError(crate::GENERR))?;
        Ok(Self(sys::switch_dtmf_t {
            digit,
            duration,
            flags: 0,
            source: source.0,
        }))
    }

    /// Sets the opaque DTMF flags (`switch_dtmf_flag_t` bitset, carried as `i32`
    /// in `switch_dtmf_t`).
    pub const fn with_flags(mut self, flags: i32) -> Self {
        self.0.flags = flags;
        self
    }

    /// The DTMF digit as a raw `c_char`.
    #[inline]
    pub fn digit_raw(self) -> c_char {
        self.0.digit
    }

    /// The DTMF digit as a `char`, or `'\0'` for a null digit.
    #[inline]
    pub fn digit(self) -> char {
        // `c_char` is a signed 8-bit integer on most platforms; widen to a `char`
        // via `u8` to avoid negative-byte surprises.
        (self.0.digit as u8) as char
    }

    /// The DTMF duration in samples.
    #[inline]
    pub fn duration(self) -> u32 {
        self.0.duration
    }

    /// The opaque DTMF flag bitset.
    #[inline]
    pub fn flags(self) -> i32 {
        self.0.flags
    }

    /// The DTMF source.
    #[inline]
    pub fn source(self) -> DtmfSource {
        DtmfSource(self.0.source)
    }

    /// The underlying `switch_dtmf_t` by value.
    #[inline]
    pub fn into_inner(self) -> sys::switch_dtmf_t {
        self.0
    }

    /// A pointer to the inner `switch_dtmf_t`, suitable for passing to a C
    /// callback expecting `*const switch_dtmf_t`.
    #[inline]
    pub fn as_ptr(&self) -> *const sys::switch_dtmf_t {
        &self.0
    }
}

/// The origin of a DTMF event (`switch_dtmf_source_t`).
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct DtmfSource(pub sys::switch_dtmf_source_t);

impl DtmfSource {
    pub const UNKNOWN: Self = Self(sys::switch_dtmf_source_t_SWITCH_DTMF_UNKNOWN);
    pub const INBAND_AUDIO: Self = Self(sys::switch_dtmf_source_t_SWITCH_DTMF_INBAND_AUDIO);
    pub const RTP: Self = Self(sys::switch_dtmf_source_t_SWITCH_DTMF_RTP);
    pub const ENDPOINT: Self = Self(sys::switch_dtmf_source_t_SWITCH_DTMF_ENDPOINT);
    pub const APP: Self = Self(sys::switch_dtmf_source_t_SWITCH_DTMF_APP);
}

/// Flags passed to `read_frame` / `write_frame` and the video/text variants
/// (`switch_io_flag_t`). This is a bitset; combine variants with `|`.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Default)]
pub struct IoFlags(pub sys::switch_io_flag_t);

impl IoFlags {
    pub const NONE: Self = Self(sys::switch_io_flag_enum_t_SWITCH_IO_FLAG_NONE);
    pub const NOBLOCK: Self = Self(sys::switch_io_flag_enum_t_SWITCH_IO_FLAG_NOBLOCK);
    pub const SINGLE_READ: Self = Self(sys::switch_io_flag_enum_t_SWITCH_IO_FLAG_SINGLE_READ);
    pub const FORCE: Self = Self(sys::switch_io_flag_enum_t_SWITCH_IO_FLAG_FORCE);
    pub const QUEUED: Self = Self(sys::switch_io_flag_enum_t_SWITCH_IO_FLAG_QUEUED);

    /// The raw bitset value.
    #[inline]
    pub const fn bits(self) -> sys::switch_io_flag_t {
        self.0
    }

    /// Returns `true` when `flag` is set in this bitset. `IoFlags::NONE`
    /// contains only itself.
    #[inline]
    pub fn contains(self, flag: Self) -> bool {
        if flag.0 == 0 {
            self.0 == 0
        } else {
            (self.0 & flag.0) == flag.0
        }
    }
}

impl std::ops::BitOr for IoFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for IoFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// A borrowed view of a FreeSWITCH session message (`switch_core_session_message_t`).
///
/// Wraps the raw pointer passed to the `receive_message` I/O callback for the
/// duration of that callback. The wrapper borrows the message and must not
/// outlive it.
#[derive(Copy, Clone)]
pub struct SessionMessage {
    raw: NonNull<sys::switch_core_session_message_t>,
}

impl SessionMessage {
    /// Wraps a session-message pointer for the duration of a `receive_message` callback.
    ///
    /// # Safety
    ///
    /// `raw` must point to a live `switch_core_session_message_t` and remain
    /// valid while this wrapper is used.
    pub unsafe fn from_raw(raw: *mut sys::switch_core_session_message_t) -> Option<Self> {
        NonNull::new(raw).map(|raw| Self { raw })
    }

    /// The raw pointer this wrapper holds.
    #[inline]
    pub fn as_ptr(&self) -> *mut sys::switch_core_session_message_t {
        self.raw.as_ptr()
    }

    /// The message type (`SWITCH_MESSAGE_*`).
    #[inline]
    pub fn message_id(&self) -> sys::switch_core_session_message_types_t {
        // SAFETY: `self.raw` is a live session message.
        unsafe { self.raw.as_ref().message_id }
    }

    /// The integer argument carried by the message.
    #[inline]
    pub fn numeric_arg(&self) -> std::os::raw::c_int {
        // SAFETY: `self.raw` is a live session message.
        unsafe { self.raw.as_ref().numeric_arg }
    }

    /// The string argument carried by the message, as an borrowed `&str` when
    /// it is valid UTF-8. Returns `None` when `string_arg` is null or not UTF-8.
    pub fn string_arg(&self) -> Option<&str> {
        // SAFETY: `self.raw` is a live session message; `string_arg` is null or a
        // null-terminated string owned by FreeSWITCH for the callback duration.
        let ptr = unsafe { self.raw.as_ref().string_arg };
        if ptr.is_null() {
            return None;
        }
        // SAFETY: `ptr` is a static-or-borrowed C string for the callback.
        unsafe { borrowed_cstr_to_str(ptr) }
    }

    /// The opaque pointer argument carried by the message.
    #[inline]
    pub fn pointer(&self) -> *mut c_void {
        // SAFETY: `self.raw` is a live session message.
        unsafe { self.raw.as_ref().pointer_arg }
    }

    /// The size, in bytes, of the pointer argument.
    #[inline]
    pub fn pointer_size(&self) -> usize {
        // SAFETY: `self.raw` is a live session message.
        unsafe { self.raw.as_ref().pointer_arg_size }
    }

    /// A mutable reference to the integer reply field, so a callback can write
    /// a numeric reply back to FreeSWITCH.
    #[inline]
    pub fn numeric_reply_mut(&mut self) -> &mut std::os::raw::c_int {
        // SAFETY: `self.raw` is live and uniquely borrowed through `&mut self`.
        unsafe { &mut self.raw.as_mut().numeric_reply }
    }

    /// A mutable reference to the string reply pointer, so a callback can set a
    /// string reply owned by the caller. FreeSWITCH reads `string_reply` and
    /// `string_reply_size` together.
    #[inline]
    pub fn string_reply_mut(&mut self) -> &mut *mut std::os::raw::c_char {
        // SAFETY: `self.raw` is live and uniquely borrowed through `&mut self`.
        unsafe { &mut self.raw.as_mut().string_reply }
    }

    /// A mutable reference to the string-reply size.
    #[inline]
    pub fn string_reply_size_mut(&mut self) -> &mut usize {
        // SAFETY: `self.raw` is live and uniquely borrowed through `&mut self`.
        unsafe { &mut self.raw.as_mut().string_reply_size }
    }

    /// A mutable reference to the pointer reply.
    #[inline]
    pub fn pointer_reply_mut(&mut self) -> &mut *mut c_void {
        // SAFETY: `self.raw` is live and uniquely borrowed through `&mut self`.
        unsafe { &mut self.raw.as_mut().pointer_reply }
    }

    /// A mutable reference to the pointer-reply size.
    #[inline]
    pub fn pointer_reply_size_mut(&mut self) -> &mut usize {
        // SAFETY: `self.raw` is live and uniquely borrowed through `&mut self`.
        unsafe { &mut self.raw.as_mut().pointer_reply_size }
    }
}

/// Builder for a FreeSWITCH `switch_io_routines` table.
///
/// Starts with every callback set to `None` (and the 10 padding slots zeroed,
/// via `switch_io_routines`'s `Default` impl). Each setter installs a typed
/// function pointer matching the corresponding `switch_io_*_t` alias from
/// `fswtch-sys`. [`IoRoutinesBuilder::build`] finalizes the table and returns
/// the raw pointer [`crate::module::ModuleBuilder::endpoint`] expects.
///
/// The built table is allocated for the module's lifetime: `build()` leaks the
/// boxed `switch_io_routines` (matching how `module.rs` leaks the
/// `StaticCStr` interface names, which are also expected to live as long as
/// the module). This is intentional — endpoint I/O routines must outlive the
/// module interface they are attached to.
#[derive(Debug, Default)]
pub struct IoRoutinesBuilder {
    inner: sys::switch_io_routines,
}

impl IoRoutinesBuilder {
    /// Creates a builder with all callbacks unset.
    pub fn new() -> Self {
        Self::default()
    }

    /// Installs the `outgoing_channel` callback.
    pub fn outgoing_channel(mut self, cb: sys::switch_io_outgoing_channel_t) -> Self {
        self.inner.outgoing_channel = cb;
        self
    }

    /// Installs the `read_frame` (audio) callback.
    pub fn read_frame(mut self, cb: sys::switch_io_read_frame_t) -> Self {
        self.inner.read_frame = cb;
        self
    }

    /// Installs the `write_frame` (audio) callback.
    pub fn write_frame(mut self, cb: sys::switch_io_write_frame_t) -> Self {
        self.inner.write_frame = cb;
        self
    }

    /// Installs the `kill_channel` callback (invoked when a session is hung up).
    pub fn kill_channel(mut self, cb: sys::switch_io_kill_channel_t) -> Self {
        self.inner.kill_channel = cb;
        self
    }

    /// Installs the `send_dtmf` callback.
    pub fn send_dtmf(mut self, cb: sys::switch_io_send_dtmf_t) -> Self {
        self.inner.send_dtmf = cb;
        self
    }

    /// Installs the `receive_message` callback.
    pub fn receive_message(mut self, cb: sys::switch_io_receive_message_t) -> Self {
        self.inner.receive_message = cb;
        self
    }

    /// Installs the `receive_event` callback.
    pub fn receive_event(mut self, cb: sys::switch_io_receive_event_t) -> Self {
        self.inner.receive_event = cb;
        self
    }

    /// Installs the `state_change` callback (session state transition).
    pub fn state_change(mut self, cb: sys::switch_io_state_change_t) -> Self {
        self.inner.state_change = cb;
        self
    }

    /// Installs the `read_video_frame` callback.
    pub fn read_video_frame(mut self, cb: sys::switch_io_read_video_frame_t) -> Self {
        self.inner.read_video_frame = cb;
        self
    }

    /// Installs the `write_video_frame` callback.
    pub fn write_video_frame(mut self, cb: sys::switch_io_write_video_frame_t) -> Self {
        self.inner.write_video_frame = cb;
        self
    }

    /// Installs the `read_text_frame` callback.
    pub fn read_text_frame(mut self, cb: sys::switch_io_read_text_frame_t) -> Self {
        self.inner.read_text_frame = cb;
        self
    }

    /// Installs the `write_text_frame` callback.
    pub fn write_text_frame(mut self, cb: sys::switch_io_write_text_frame_t) -> Self {
        self.inner.write_text_frame = cb;
        self
    }

    /// Installs the `state_run` callback (per-state custom run handler).
    pub fn state_run(mut self, cb: sys::switch_io_state_run_t) -> Self {
        self.inner.state_run = cb;
        self
    }

    /// Installs the `get_jb` callback (jitter-buffer accessor).
    pub fn get_jb(mut self, cb: sys::switch_io_get_jb_t) -> Self {
        self.inner.get_jb = cb;
        self
    }

    /// Finalizes the I/O routines table, returning the raw pointer to pass to
    /// [`crate::module::ModuleBuilder::endpoint`].
    ///
    /// The returned pointer is valid for the remainder of the program (the
    /// table is intentionally leaked, matching the module-lifetime expectation of
    /// FreeSWITCH endpoint interfaces). Repeated calls return distinct
    /// allocations; do not free the pointer.
    pub fn build(self) -> Result<*mut sys::switch_io_routines_t> {
        let boxed = Box::new(self.inner);
        let ptr = Box::into_raw(boxed);
        // SAFETY: `ptr` is a valid, well-aligned, owned allocation produced by
        // `Box::into_raw`; it remains valid for the program lifetime by design.
        Ok(ptr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dtmf_roundtrip() {
        let d = Dtmf::new('#', 1600, DtmfSource::APP).unwrap().with_flags(0);
        assert_eq!(d.digit(), '#');
        assert_eq!(d.duration(), 1600);
        assert_eq!(d.source(), DtmfSource::APP);
    }

    #[test]
    fn io_flags_combine() {
        let f = IoFlags::NOBLOCK | IoFlags::SINGLE_READ;
        assert!(f.contains(IoFlags::NOBLOCK));
        assert!(f.contains(IoFlags::SINGLE_READ));
        assert!(!f.contains(IoFlags::FORCE));
    }

    #[test]
    fn builder_starts_empty() {
        let b = IoRoutinesBuilder::new();
        assert!(b.inner.outgoing_channel.is_none());
        assert!(b.inner.read_frame.is_none());
        assert!(b.inner.get_jb.is_none());
        // padding is zeroed by Default
        assert!(b.inner.padding.iter().all(|p| p.is_null()));
    }

    #[test]
    fn build_returns_valid_pointer() {
        unsafe extern "C" fn noop_kill(
            _: *mut sys::switch_core_session_t,
            _: std::os::raw::c_int,
        ) -> crate::Status {
            crate::SUCCESS
        }
        let cb: sys::switch_io_kill_channel_t = Some(noop_kill);
        let ptr = IoRoutinesBuilder::new()
            .kill_channel(cb)
            .build()
            .expect("build");
        // SAFETY: `ptr` is a leaked boxed `switch_io_routines` produced by `build`.
        let r = unsafe { &*ptr };
        assert!(r.kill_channel.is_some());
        assert!(r.read_frame.is_none());
        assert!(r.padding.iter().all(|p| p.is_null()));
        // SAFETY: reclaim the leak so the test does not leak under Miri.
        unsafe { drop(Box::from_raw(ptr)) };
    }
}
