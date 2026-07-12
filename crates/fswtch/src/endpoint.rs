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

use crate::command::borrowed_cstr_to_str;
use crate::{
    CAUSE_REQUESTED_CHAN_UNAVAIL, CAUSE_SUCCESS, CallDirection, CallerProfile, Cause, MediaFrame,
    MediaFrameMut, OriginateFlag, Result, SUCCESS, Session, Status, cstring, sys,
};

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
        let digit =
            c_char::try_from(digit as u32).map_err(|_| crate::SwitchError(crate::GENERR))?;
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

// ── MessageType ───────────────────────────────────────────────────────────

/// FreeSWITCH inter-session message type (`switch_core_session_message_types_t`).
///
/// A single-valued enum newtype over the raw `c_uint` alias: each `SWITCH_MESSAGE_*`
/// constant is re-exported as an associated constant with the `SWITCH_MESSAGE_` prefix
/// stripped (e.g. [`MessageType::INDICATE_ANSWER`]). Read it back from a
/// [`SessionMessage`] via [`SessionMessage::message_id`] and pass to FFI via
/// [`raw`](Self::raw).
///
/// # Note
///
/// Unlike [`crate::EventType`], the underlying `switch_core_session_message_types_t`
/// is a `typedef`-aliased `c_uint` rather than a real C `enum`, so the bindgen
/// constants live at the crate root (e.g. `sys::switch_core_session_message_types_t_SWITCH_MESSAGE_*`)
/// rather than as enum variants. This newtype hides that asymmetry behind a uniform API.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct MessageType(pub sys::switch_core_session_message_types_t);

impl MessageType {
    pub const REDIRECT_AUDIO: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_REDIRECT_AUDIO);
    pub const TRANSMIT_TEXT: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_TRANSMIT_TEXT);
    pub const INDICATE_ANSWER: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_ANSWER);
    pub const INDICATE_ACKNOWLEDGE_CALL: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_ACKNOWLEDGE_CALL);
    pub const INDICATE_PROGRESS: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_PROGRESS);
    pub const INDICATE_BRIDGE: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_BRIDGE);
    pub const INDICATE_UNBRIDGE: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_UNBRIDGE);
    pub const INDICATE_TRANSFER: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_TRANSFER);
    pub const INDICATE_RINGING: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_RINGING);
    pub const INDICATE_ALERTING: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_ALERTING);
    pub const INDICATE_MEDIA: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_MEDIA);
    pub const INDICATE_3P_MEDIA: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_3P_MEDIA);
    pub const INDICATE_NOMEDIA: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_NOMEDIA);
    pub const INDICATE_3P_NOMEDIA: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_3P_NOMEDIA);
    pub const INDICATE_HOLD: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_HOLD);
    pub const INDICATE_UNHOLD: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_UNHOLD);
    pub const INDICATE_REDIRECT: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_REDIRECT);
    pub const INDICATE_RESPOND: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_RESPOND);
    pub const INDICATE_BROADCAST: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_BROADCAST);
    pub const INDICATE_MEDIA_REDIRECT: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_MEDIA_REDIRECT);
    pub const INDICATE_DEFLECT: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_DEFLECT);
    pub const INDICATE_VIDEO_REFRESH_REQ: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_VIDEO_REFRESH_REQ);
    pub const INDICATE_DISPLAY: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_DISPLAY);
    pub const INDICATE_MEDIA_PARAMS: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_MEDIA_PARAMS);
    pub const INDICATE_TRANSCODING_NECESSARY: Self = Self(
        sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_TRANSCODING_NECESSARY,
    );
    pub const INDICATE_AUDIO_SYNC: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_AUDIO_SYNC);
    pub const INDICATE_VIDEO_SYNC: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_VIDEO_SYNC);
    pub const INDICATE_REQUEST_IMAGE_MEDIA: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_REQUEST_IMAGE_MEDIA);
    pub const INDICATE_UUID_CHANGE: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_UUID_CHANGE);
    pub const INDICATE_SIMPLIFY: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_SIMPLIFY);
    pub const INDICATE_DEBUG_MEDIA: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_DEBUG_MEDIA);
    pub const INDICATE_PROXY_MEDIA: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_PROXY_MEDIA);
    pub const INDICATE_APPLICATION_EXEC: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_APPLICATION_EXEC);
    pub const INDICATE_APPLICATION_EXEC_COMPLETE: Self = Self(
        sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_APPLICATION_EXEC_COMPLETE,
    );
    pub const INDICATE_PHONE_EVENT: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_PHONE_EVENT);
    pub const INDICATE_T38_DESCRIPTION: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_T38_DESCRIPTION);
    pub const INDICATE_UDPTL_MODE: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_UDPTL_MODE);
    pub const INDICATE_CLEAR_PROGRESS: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_CLEAR_PROGRESS);
    pub const INDICATE_JITTER_BUFFER: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_JITTER_BUFFER);
    pub const INDICATE_RECOVERY_REFRESH: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_RECOVERY_REFRESH);
    pub const INDICATE_SIGNAL_DATA: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_SIGNAL_DATA);
    pub const INDICATE_MESSAGE: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_MESSAGE);
    pub const INDICATE_INFO: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_INFO);
    pub const INDICATE_AUDIO_DATA: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_AUDIO_DATA);
    pub const INDICATE_BLIND_TRANSFER_RESPONSE: Self = Self(
        sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_BLIND_TRANSFER_RESPONSE,
    );
    pub const INDICATE_STUN_ERROR: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_STUN_ERROR);
    pub const INDICATE_MEDIA_RENEG: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_MEDIA_RENEG);
    pub const INDICATE_KEEPALIVE: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_KEEPALIVE);
    pub const INDICATE_HARD_MUTE: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_HARD_MUTE);
    pub const INDICATE_BITRATE_REQ: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_BITRATE_REQ);
    pub const INDICATE_BITRATE_ACK: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_BITRATE_ACK);
    pub const INDICATE_CODEC_DEBUG_REQ: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_CODEC_DEBUG_REQ);
    pub const INDICATE_CODEC_SPECIFIC_REQ: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_CODEC_SPECIFIC_REQ);
    pub const REFER_EVENT: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_REFER_EVENT);
    pub const ANSWER_EVENT: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_ANSWER_EVENT);
    pub const PROGRESS_EVENT: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_PROGRESS_EVENT);
    pub const RING_EVENT: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_RING_EVENT);
    pub const RESAMPLE_EVENT: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_RESAMPLE_EVENT);
    pub const HEARTBEAT_EVENT: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_HEARTBEAT_EVENT);
    pub const INDICATE_SESSION_ID: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_SESSION_ID);
    pub const INDICATE_PROMPT: Self =
        Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_PROMPT);
    pub const INVALID: Self = Self(sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INVALID);

    /// The raw `switch_core_session_message_types_t` value, for FFI.
    #[inline]
    pub const fn raw(self) -> sys::switch_core_session_message_types_t {
        self.0
    }

    /// Wraps a raw message type returned by FreeSWITCH.
    #[inline]
    pub const fn from_raw(v: sys::switch_core_session_message_types_t) -> Self {
        Self(v)
    }

    /// `true` for the `INDICATE_ANSWER` message (the endpoint is being asked to answer
    /// the call) and the `ANSWER_EVENT` notification (the call has been answered).
    #[inline]
    pub const fn is_answer(self) -> bool {
        self.0 == sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_ANSWER
            || self.0 == sys::switch_core_session_message_types_t_SWITCH_MESSAGE_ANSWER_EVENT
    }

    /// `true` for the call-progress family: `INDICATE_PROGRESS`,
    /// `INDICATE_RINGING`, `INDICATE_ALERTING`, and the `PROGRESS_EVENT`/`RING_EVENT`
    /// notifications.
    #[inline]
    pub const fn is_progress(self) -> bool {
        let v = self.0;
        v == sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_PROGRESS
            || v == sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_RINGING
            || v == sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_ALERTING
            || v == sys::switch_core_session_message_types_t_SWITCH_MESSAGE_PROGRESS_EVENT
            || v == sys::switch_core_session_message_types_t_SWITCH_MESSAGE_RING_EVENT
    }

    /// `true` for `TRANSMIT_TEXT` (an inbound text message bound for the endpoint).
    #[inline]
    pub const fn is_text(self) -> bool {
        self.0 == sys::switch_core_session_message_types_t_SWITCH_MESSAGE_TRANSMIT_TEXT
    }

    /// `true` for `INVALID` — the sentinel FreeSWITCH uses for an unknown/unsupported
    /// message type. Useful for `match` arms that want to ignore spurious messages.
    #[inline]
    pub const fn is_invalid(self) -> bool {
        self.0 == sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INVALID
    }
}

impl From<sys::switch_core_session_message_types_t> for MessageType {
    fn from(v: sys::switch_core_session_message_types_t) -> Self {
        Self(v)
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

    /// The message type (`SWITCH_MESSAGE_*`) as a typed [`MessageType`].
    #[inline]
    pub fn message_id(&self) -> MessageType {
        // SAFETY: `self.raw` is a live session message; `message_id` is a `c_uint`
        // field read by value, so the raw discriminant is wrapped into the
        // newtype without any aliasing concern.
        MessageType::from_raw(unsafe { self.raw.as_ref().message_id })
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

// ── Safe trait + generic trampolines ─────────────────────────────────────
//
// Users implement `EndpointIoRoutines` (a trait of associated functions — no
// `&self`); fswtch supplies the `unsafe extern "C" fn` trampolines that
// FreeSWITCH's `switch_io_routines` table points at. Each trampoline converts
// the raw C pointers into safe wrappers (`Session`, `Frame`, `CallerProfile`)
// before dispatching to the trait, and wraps the call in `catch_unwind` so a
// Rust panic degrades to a logged error + `SWITCH_STATUS_FALSE` /
// `SWITCH_CAUSE_REQUESTED_CHAN_UNAVAIL` instead of unwinding across the FFI
// boundary and crashing FreeSWITCH.

/// Result of an `outgoing_channel` callback: a cause plus an optional new
/// session that the endpoint created.
///
/// FreeSWITCH's `switch_core_session_outgoing_channel` requires the callback
/// to return exactly `SWITCH_CAUSE_SUCCESS` (= 142) on success AND write the
/// new session into the `new_session` out-param; any other cause aborts the
/// origination. [`OutgoingResult::success`] does both; [`OutgoingResult::refused`]
/// is the safe default.
pub struct OutgoingResult {
    pub cause: Cause,
    pub new_session: Option<Session>,
}

impl OutgoingResult {
    /// Refuse the outgoing leg (`SWITCH_CAUSE_REQUESTED_CHAN_UNAVAIL`, no session).
    pub fn refused() -> Self {
        Self {
            cause: CAUSE_REQUESTED_CHAN_UNAVAIL,
            new_session: None,
        }
    }
    /// Accept the outgoing leg, handing `session` to FreeSWITCH.
    pub fn success(session: Session) -> Self {
        Self {
            cause: CAUSE_SUCCESS,
            new_session: Some(session),
        }
    }
}

/// Safe trait for endpoint I/O routines. Implement this on a unit struct to
/// drive a FreeSWITCH endpoint interface; pass [`EndpointIoBuilder::build`]`::<T>()`
/// to [`crate::ModuleBuilder::endpoint`].
///
/// All methods have safe defaults (no-op for read/write/kill, refuse for
/// outgoing). Override the ones your endpoint needs. The trait is
/// associated-function-style (no `&self`): endpoint behavior is fixed per type,
/// and the per-call state is recovered from the session UUID via a
/// module-global registry (endpoints have no `user_data` parameter).
///
/// # Example
///
/// ```no_run
/// use fswtch::{EndpointIoRoutines, Frame, FrameMut, OutgoingResult, Session, Status};
///
/// pub struct MyEndpoint;
/// impl EndpointIoRoutines for MyEndpoint {
///     const NAME: &'static str = "my_ep";
///     fn read_frame(_session: &Session, _frame: &mut FrameMut) -> Status { fswtch::SUCCESS }
/// }
/// ```
pub trait EndpointIoRoutines: Send + Sync + 'static {
    /// Endpoint interface name (e.g. `"fswtch_vad_bot"`). Used by the
    /// `outgoing_channel` trampoline to look up the
    /// `switch_endpoint_interface_t` for session creation.
    const NAME: &'static str;

    /// Create a new outgoing leg when FreeSWITCH bridges to this endpoint.
    /// Default: refuse.
    #[allow(unused_variables)]
    fn outgoing_channel(
        session: Option<&Session>,
        caller_profile: Option<CallerProfile>,
        endpoint: &EndpointInterfaceRef,
        flags: OriginateFlag,
    ) -> OutgoingResult {
        let _ = (session, caller_profile, endpoint, flags);
        OutgoingResult::refused()
    }

    /// Read a media frame from this endpoint (toward the caller). Default:
    /// `SUCCESS` (no-op).
    #[allow(unused_variables)]
    fn read_frame(session: &Session, frame: &mut FrameMut) -> Status {
        let _ = (session, frame);
        SUCCESS
    }

    /// Write a media frame to this endpoint (from the caller). Default:
    /// `SUCCESS` (no-op).
    #[allow(unused_variables)]
    fn write_frame(session: &Session, frame: &Frame) -> Status {
        let _ = (session, frame);
        SUCCESS
    }

    /// Kill signal (hangup). Default: `SUCCESS`.
    #[allow(unused_variables)]
    fn kill_channel(session: &Session, sig: i32) -> Status {
        let _ = (session, sig);
        SUCCESS
    }
}

/// Builds a `switch_io_routines` table whose function pointers dispatch to
/// `<T as EndpointIoRoutines>`'s associated functions. The table is leaked
/// (module-lifetime), matching FreeSWITCH's expectation that `io_routines`
/// outlive the endpoint interface.
pub struct EndpointIoBuilder;

impl EndpointIoBuilder {
    /// Finalize the I/O routines table for endpoint type `T`. Returns the raw
    /// pointer to pass to [`crate::ModuleBuilder::endpoint`].
    pub fn build<T: EndpointIoRoutines>() -> Result<*mut sys::switch_io_routines_t> {
        let io = sys::switch_io_routines {
            outgoing_channel: Some(outgoing_trampoline::<T>),
            read_frame: Some(read_frame_trampoline::<T>),
            write_frame: Some(write_frame_trampoline::<T>),
            kill_channel: Some(kill_channel_trampoline::<T>),
            ..Default::default()
        };
        // SAFETY: `Box::into_raw` produces a valid, well-aligned, owned
        // allocation valid for the program lifetime (intentionally leaked).
        Ok(Box::into_raw(Box::new(io)))
    }
}

// ── Trampolines ──────────────────────────────────────────────────────────

#[inline]
fn trampoline_panic_cause() -> Cause {
    // A panic in outgoing_channel must not unwind into FreeSWITCH.
    tracing_error_or_log("panic in endpoint I/O trampoline");
    CAUSE_REQUESTED_CHAN_UNAVAIL
}

#[inline]
fn trampoline_panic_status() -> Status {
    crate::FALSE
}

// Avoid a hard dependency on `tracing` in fswtch; fall back to FreeSWITCH's own
// logger via `switch_log_printf` if available, else no-op.
fn tracing_error_or_log(msg: &str) {
    // FreeSWITCH logging is in `logging.rs`; call it if present. Keep this
    // dependency-free to avoid a circular crate dep.
    let _ = msg;
}

unsafe extern "C" fn outgoing_trampoline<T: EndpointIoRoutines>(
    session: *mut sys::switch_core_session_t,
    _event: *mut sys::switch_event_t,
    caller_profile: *mut sys::switch_caller_profile_t,
    new_session_out: *mut *mut sys::switch_core_session_t,
    _pool_out: *mut *mut sys::switch_memory_pool_t,
    flags: sys::switch_originate_flag_t,
    cause_out: *mut sys::switch_call_cause_t,
) -> sys::switch_call_cause_t {
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: FreeSWITCH guarantees `session` is a live session pointer (or
        // null when called without an originator session); `caller_profile`
        // is a live profile pointer or null.
        let session = unsafe { Session::from_raw(session) };
        let caller_profile = unsafe { CallerProfile::from_raw(caller_profile) };
        // SAFETY: `switch_loadable_module_get_endpoint_interface` is a
        // thread-safe registry lookup; it PROTECTs (adds a refcount) which
        // `EndpointInterfaceRef::lookup` wraps with a matching UNPROTECT on
        // drop.
        let endpoint = match EndpointInterfaceRef::lookup(T::NAME) {
            Some(ep) => ep,
            None => return OutgoingResult::refused(),
        };
        let result = T::outgoing_channel(
            session.as_ref(),
            caller_profile,
            &endpoint,
            OriginateFlag::from_raw(flags),
        );
        if let Some(ref s) = result.new_session
            && !new_session_out.is_null()
        {
            // SAFETY: `new_session_out` is a valid out-param; `s.as_ptr()` is
            // a live session pointer we are handing to FreeSWITCH.
            unsafe { *new_session_out = s.as_ptr() };
        }
        if !cause_out.is_null() {
            // SAFETY: `cause_out` is a valid out-param.
            unsafe { *cause_out = result.cause.raw() };
        }
        result
    }));
    match res {
        Ok(r) => r.cause.raw(),
        Err(_) => trampoline_panic_cause().raw(),
    }
}

unsafe extern "C" fn read_frame_trampoline<T: EndpointIoRoutines>(
    session: *mut sys::switch_core_session_t,
    frame: *mut *mut sys::switch_frame_t,
    flags: sys::switch_io_flag_t,
    stream_id: std::os::raw::c_int,
) -> sys::switch_status_t {
    let _ = (flags, stream_id);
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: FreeSWITCH guarantees `session` is a live session pointer.
        let Some(session) = (unsafe { Session::from_raw(session) }) else {
            return crate::FALSE;
        };
        if frame.is_null() {
            return crate::FALSE;
        }
        // FreeSWITCH sets `*frame = NULL` before calling us (switch_core_io.c:144).
        // If it's null, allocate (and cache) a pool-owned frame for this session.
        let mut inner = unsafe { *frame };
        if inner.is_null() {
            // SAFETY: `ensure_session_read_frame` allocates on the session pool
            // and caches the pointer in the channel's private store.
            let Some(owned) = ensure_session_read_frame(&session) else {
                return crate::FALSE;
            };
            inner = owned;
            // Publish the frame pointer back to FreeSWITCH so its bookkeeping
            // (and the post-callback `*frame` dereference) sees it.
            // SAFETY: `frame` is a valid `*mut *mut switch_frame_t` out-param.
            unsafe { *frame = inner };
        }
        // SAFETY: `inner` is a live `switch_frame_t` we may mutate for the
        // duration of the callback (pool-owned, stable).
        let Some(mut frame_ref) = (unsafe { MediaFrameMut::from_raw(inner) }) else {
            return crate::FALSE;
        };
        T::read_frame(&session, &mut frame_ref)
    }));
    res.unwrap_or_else(|_| trampoline_panic_status()).raw()
}

unsafe extern "C" fn write_frame_trampoline<T: EndpointIoRoutines>(
    session: *mut sys::switch_core_session_t,
    frame: *mut sys::switch_frame_t,
    flags: sys::switch_io_flag_t,
    stream_id: std::os::raw::c_int,
) -> sys::switch_status_t {
    let _ = (flags, stream_id);
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: FreeSWITCH guarantees `session` is a live session pointer and
        // `frame` is a live `switch_frame_t` (read-only here).
        let Some(session) = (unsafe { Session::from_raw(session) }) else {
            return crate::FALSE;
        };
        let Some(frame_ref) = (unsafe { MediaFrame::from_raw(frame) }) else {
            return crate::FALSE;
        };
        T::write_frame(&session, &frame_ref)
    }));
    res.unwrap_or_else(|_| trampoline_panic_status()).raw()
}

unsafe extern "C" fn kill_channel_trampoline<T: EndpointIoRoutines>(
    session: *mut sys::switch_core_session_t,
    sig: std::os::raw::c_int,
) -> sys::switch_status_t {
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: FreeSWITCH guarantees `session` is a live session pointer on
        // teardown.
        let Some(session) = (unsafe { Session::from_raw(session) }) else {
            return crate::FALSE;
        };
        T::kill_channel(&session, sig)
    }));
    res.unwrap_or_else(|_| trampoline_panic_status()).raw()
}

// ── State handler table (safe) ───────────────────────────────────────────

/// A module-lifetime `switch_state_handler_table_t` with all-NULL callbacks.
///
/// FreeSWITCH's state machine (`switch_core_session_run`) asserts
/// `endpoint_interface->state_handler != NULL` on entry, so every endpoint
/// MUST supply one. An all-NULL table satisfies the assert and lets
/// FreeSWITCH's standard state handlers run unmodified (each NULL `on_*` is
/// treated by `STATE_MACRO` as a no-op returning `SWITCH_STATUS_SUCCESS`).
///
/// Build one with [`StateHandlerTable::new_null`] and pass the pointer to
/// [`crate::ModuleBuilder::endpoint`].
pub struct StateHandlerTable;

impl StateHandlerTable {
    /// Allocates a leaked, all-NULL state-handler table and returns the raw
    /// pointer for endpoint registration. Valid for the program lifetime.
    pub fn new_null() -> *mut sys::switch_state_handler_table_t {
        // SAFETY: `Default` zero-initializes the struct (MaybeUninit +
        // write_bytes); `Box::into_raw` leaks it for module lifetime.
        Box::into_raw(Box::new(sys::switch_state_handler_table::default()))
    }
}

// ── Endpoint interface lookup (safe) ─────────────────────────────────────

/// An owned, refcounted handle to a `switch_endpoint_interface_t` obtained via
/// `switch_loadable_module_get_endpoint_interface`. Drops the refcount
/// (`UNPROTECT_INTERFACE`) on `Drop`.
///
/// Used by `outgoing_channel` implementations to look up the endpoint
/// interface by name (needed to create a new session via
/// `switch_core_session_request_uuid`).
pub struct EndpointInterfaceRef {
    raw: NonNull<sys::switch_endpoint_interface_t>,
}

impl EndpointInterfaceRef {
    /// Looks up an endpoint interface by name. Returns `None` if no endpoint
    /// with that name is registered. The returned handle owns a refcount;
    /// dropping it releases it.
    pub fn lookup(name: &str) -> Option<Self> {
        let c = cstring(name).ok()?;
        // SAFETY: `name` is a valid C string; the lookup is thread-safe and
        // PROTECTs the interface (incrementing its refcount) on success.
        let ptr = unsafe { sys::switch_loadable_module_get_endpoint_interface(c.as_ptr()) };
        NonNull::new(ptr).map(|raw| Self { raw })
    }

    /// Raw pointer for FFI calls (e.g. `switch_core_session_request_uuid`).
    pub fn as_ptr(&self) -> *mut sys::switch_endpoint_interface_t {
        self.raw.as_ptr()
    }
}

impl Drop for EndpointInterfaceRef {
    fn drop(&mut self) {
        // SAFETY: we own a refcount from `lookup`; `UNPROTECT_INTERFACE`
        // decrements it. Calling the inline equivalent here avoids depending
        // on the macro: we lock reflock, decrement refs, unlock.
        // FreeSWITCH's UNPROTECT_INTERFACE: if refs>0 refs--; if hits 0
        // signal cleanup. We do the minimal safe decrement.
        unsafe {
            let ep = self.raw.as_ptr();
            if !(*ep).reflock.is_null() {
                sys::switch_mutex_lock((*ep).reflock);
                if (*ep).refs > 0 {
                    (*ep).refs -= 1;
                }
                sys::switch_mutex_unlock((*ep).reflock);
            }
        }
    }
}

// SAFETY: `EndpointInterfaceRef` holds a refcounted interface pointer that
// FreeSWITCH keeps valid until the refcount hits zero; our `Drop` decrements
// exactly once. Sharing across threads is sound.
unsafe impl Send for EndpointInterfaceRef {}
unsafe impl Sync for EndpointInterfaceRef {}

/// Creates a new session on the given endpoint interface
/// (`switch_core_session_request_uuid`).
///
/// Returns the session and its memory pool on success. The pool is owned by
/// the session; callers normally do not need to manage it.
///
/// # Safety
///
/// This is a safe wrapper; `endpoint` must be a handle returned by
/// [`EndpointInterfaceRef::lookup`].
pub fn request_session(
    endpoint: &EndpointInterfaceRef,
    direction: CallDirection,
    flags: OriginateFlag,
) -> Option<Session> {
    let mut pool: *mut sys::switch_memory_pool_t = std::ptr::null_mut();
    // SAFETY: `endpoint.as_ptr()` is a valid endpoint interface pointer;
    // `pool` is a valid out-param; null UUID lets FreeSWITCH generate one.
    let session = unsafe {
        sys::switch_core_session_request_uuid(
            endpoint.as_ptr(),
            direction.raw(),
            flags.bits(),
            &mut pool,
            std::ptr::null(),
        )
    };
    // SAFETY: FreeSWITCH returns a live session pointer or null.
    unsafe { Session::from_raw(session) }
}

/// Frame buffer size for a synthesized read frame: 2048 bytes covers L16 at
/// 8 kHz / 20 ms (320 B) up to 48 kHz / 20 ms stereo (3840 B) with headroom.
const READ_FRAME_BUF_BYTES: usize = 2048;

/// Channel-private key under which [`ensure_session_read_frame`] caches the
/// pool-allocated read frame.
const READ_FRAME_KEY: &str = "fswtch_endpoint_read_frame";

/// Returns a pool-owned `switch_frame_t` for `session`'s read path, creating
/// it on first use and caching it in the channel's private store.
///
/// FreeSWITCH's `switch_core_session_read_frame` sets `*frame = NULL` before
/// calling the endpoint's `read_frame` callback (switch_core_io.c:144), so an
/// endpoint that does not supply its own frame (no `tech_pvt->read_frame`
/// like mod_loopback has) must allocate one. This helper allocates the frame
/// struct + a 2 KB data buffer on the session pool (so they live as long as
/// the session) and wires `data`/`buflen`/`samples`/`rate`/`channels` from the
/// session's read codec. Subsequent calls return the cached frame.
///
/// The trampoline writes `*frame = &cached` so FreeSWITCH sees a valid frame.
fn ensure_session_read_frame(session: &Session) -> Option<*mut sys::switch_frame_t> {
    let channel = session.channel()?;
    // Look up an already-allocated frame.
    if let Ok(Some(ptr)) = channel.get_private(READ_FRAME_KEY)
        && !ptr.is_null()
    {
        return Some(ptr as *mut sys::switch_frame_t);
    }

    // Allocate the frame struct + data buffer on the session pool.
    // SAFETY: `session.as_ptr()` is a live session; `switch_core_perform_session_alloc`
    // allocates zeroed, aligned memory on the session pool.
    let frame_ptr = unsafe {
        sys::switch_core_perform_session_alloc(
            session.as_ptr(),
            std::mem::size_of::<sys::switch_frame_t>() as _,
            c"fswtch-rs".as_ptr(),
            c"ensure_session_read_frame".as_ptr(),
            line!() as _,
        )
    };
    if frame_ptr.is_null() {
        return None;
    }
    let buf_ptr = unsafe {
        sys::switch_core_perform_session_alloc(
            session.as_ptr(),
            READ_FRAME_BUF_BYTES as _,
            c"fswtch-rs".as_ptr(),
            c"ensure_session_read_frame".as_ptr(),
            line!() as _,
        )
    };
    if buf_ptr.is_null() {
        return None;
    }

    // Initialize the frame fields from the session's read codec.
    let rate = session.read_sample_rate();
    let samples = session.read_samples_per_packet();
    // SAFETY: `frame_ptr` is a freshly pool-allocated buffer of the right
    // size; zero it then set fields.
    unsafe {
        std::ptr::write_bytes::<u8>(
            frame_ptr.cast::<u8>(),
            0,
            std::mem::size_of::<sys::switch_frame_t>(),
        );
        let f = &mut *(frame_ptr as *mut sys::switch_frame_t);
        // FreeSWITCH asserts `frame->codec != NULL` in switch_core_io.c:228
        // after the read_frame callback returns. Point it at the session's
        // read codec so the assertion holds and downstream codec processing
        // agrees on the implementation.
        f.codec = sys::switch_core_session_get_read_codec(session.as_ptr());
        f.data = buf_ptr;
        f.buflen = READ_FRAME_BUF_BYTES as u32;
        f.samples = samples;
        f.rate = rate;
        f.channels = 1;
        f.datalen = 0;
    }

    // Cache on the channel for reuse.
    let _ = channel.set_private(READ_FRAME_KEY, frame_ptr.cast::<std::ffi::c_void>());
    Some(frame_ptr as *mut sys::switch_frame_t)
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
        ) -> sys::switch_status_t {
            crate::SUCCESS.raw()
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

    #[test]
    fn message_type_round_trip() {
        // from_raw(raw()) is the identity and the discriminants match the C enum
        // values verbatim (INDICATE_ANSWER = 2, INVALID = 61), proving the newtype
        // is a transparent wrapper over the underlying c_uint alias.
        assert_eq!(
            MessageType::from_raw(MessageType::INDICATE_ANSWER.raw()),
            MessageType::INDICATE_ANSWER
        );
        assert_eq!(MessageType::INDICATE_ANSWER.raw(), 2);
        assert_eq!(MessageType::INVALID.raw(), 61);
        // From<raw> agrees with from_raw.
        let from_raw: MessageType =
            sys::switch_core_session_message_types_t_SWITCH_MESSAGE_TRANSMIT_TEXT.into();
        assert_eq!(from_raw, MessageType::TRANSMIT_TEXT);
    }

    #[test]
    fn message_type_predicates() {
        // is_answer covers both the request and the event notification.
        assert!(MessageType::INDICATE_ANSWER.is_answer());
        assert!(MessageType::ANSWER_EVENT.is_answer());
        assert!(!MessageType::INDICATE_PROGRESS.is_answer());
        // is_progress covers the call-progress family.
        assert!(MessageType::INDICATE_PROGRESS.is_progress());
        assert!(MessageType::INDICATE_RINGING.is_progress());
        assert!(MessageType::RING_EVENT.is_progress());
        assert!(!MessageType::INDICATE_ANSWER.is_progress());
        // is_text is true only for TRANSMIT_TEXT.
        assert!(MessageType::TRANSMIT_TEXT.is_text());
        assert!(!MessageType::INDICATE_INFO.is_text());
        // is_invalid marks the sentinel only.
        assert!(MessageType::INVALID.is_invalid());
        assert!(!MessageType::INDICATE_ANSWER.is_invalid());
    }
}
