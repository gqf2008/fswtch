//! FreeSWITCH enum wrappers: `Status`, `Cause`, `ChannelState`, `CallDirection`,
//! `OriginateFlag`.
//!
//! Each is a `#[derive(Copy, Clone, …)] pub struct T(pub(crate) sys::RawT)` newtype wrapping the
//! bindgen-generated C enum/underlying int. This gives:
//!
//! - **Type safety** — `Status` and `Cause` are distinct newtypes; they cannot be mixed even
//!   though both ultimately hold an integer. (The old `pub type = sys::…` aliases all
//!   collapsed to `u32`/`i32` and silently let any value flow between them.)
//! - **Associated constants** — variants live under their type (`Status::SUCCESS`,
//!   `Cause::SUCCESS`, `ChannelState::CONSUME_MEDIA`), not as free `SWITCH_STATUS_*`-prefixed
//!   module constants. IDE completion and rustdoc group them naturally.
//! - **FFI zero-cost** — `.0` (or `.raw()` / `.bits()`) is the bindgen type, passed straight to
//!   `sys::` calls with no transmute or match mapping.
//!
//! Single-value enums expose `raw()` / `from_raw()`; bitmasks (`OriginateFlag`) expose
//! `bits()` / `contains()` and `BitOr`/`BitOrAssign`, matching the existing `IoFlags` /
//! `MediaBugFlags` pattern elsewhere in the crate.

use std::{error::Error, fmt};

use crate::sys;

// ── Status ───────────────────────────────────────────────────────────────

/// FreeSWITCH function return status (`switch_status_t`).
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct Status(pub(crate) sys::switch_status_t);

impl Status {
    pub const SUCCESS: Self = Self(sys::switch_status_t::SWITCH_STATUS_SUCCESS);
    pub const FALSE: Self = Self(sys::switch_status_t::SWITCH_STATUS_FALSE);
    pub const BREAK: Self = Self(sys::switch_status_t::SWITCH_STATUS_BREAK);
    pub const CONTINUE: Self = Self(sys::switch_status_t::SWITCH_STATUS_CONTINUE);
    pub const FOUND: Self = Self(sys::switch_status_t::SWITCH_STATUS_FOUND);
    pub const GENERR: Self = Self(sys::switch_status_t::SWITCH_STATUS_GENERR);
    pub const IGNORE: Self = Self(sys::switch_status_t::SWITCH_STATUS_IGNORE);
    pub const INTR: Self = Self(sys::switch_status_t::SWITCH_STATUS_INTR);
    pub const INUSE: Self = Self(sys::switch_status_t::SWITCH_STATUS_INUSE);
    pub const MEMERR: Self = Self(sys::switch_status_t::SWITCH_STATUS_MEMERR);
    pub const MORE_DATA: Self = Self(sys::switch_status_t::SWITCH_STATUS_MORE_DATA);
    pub const NOOP: Self = Self(sys::switch_status_t::SWITCH_STATUS_NOOP);
    pub const NOT_INITALIZED: Self = Self(sys::switch_status_t::SWITCH_STATUS_NOT_INITALIZED);
    pub const NOTFOUND: Self = Self(sys::switch_status_t::SWITCH_STATUS_NOTFOUND);
    pub const NOTIMPL: Self = Self(sys::switch_status_t::SWITCH_STATUS_NOTIMPL);
    pub const NOUNLOAD: Self = Self(sys::switch_status_t::SWITCH_STATUS_NOUNLOAD);
    pub const RESAMPLE: Self = Self(sys::switch_status_t::SWITCH_STATUS_RESAMPLE);
    pub const RESTART: Self = Self(sys::switch_status_t::SWITCH_STATUS_RESTART);
    pub const SOCKERR: Self = Self(sys::switch_status_t::SWITCH_STATUS_SOCKERR);
    pub const TERM: Self = Self(sys::switch_status_t::SWITCH_STATUS_TERM);
    pub const TIMEOUT: Self = Self(sys::switch_status_t::SWITCH_STATUS_TIMEOUT);
    pub const TOO_LATE: Self = Self(sys::switch_status_t::SWITCH_STATUS_TOO_LATE);
    pub const TOO_SMALL: Self = Self(sys::switch_status_t::SWITCH_STATUS_TOO_SMALL);
    pub const UNLOAD: Self = Self(sys::switch_status_t::SWITCH_STATUS_UNLOAD);
    pub const WINBREAK: Self = Self(sys::switch_status_t::SWITCH_STATUS_WINBREAK);
    pub const XBREAK: Self = Self(sys::switch_status_t::SWITCH_STATUS_XBREAK);

    /// The raw `switch_status_t` value, for FFI.
    #[inline]
    pub(crate) const fn raw(self) -> sys::switch_status_t {
        self.0
    }

    /// Wraps a raw `switch_status_t` returned by FreeSWITCH.
    #[inline]
    pub(crate) const fn from_raw(v: sys::switch_status_t) -> Self {
        Self(v)
    }

    /// `true` when this is `SUCCESS`.
    #[inline]
    pub const fn is_success(self) -> bool {
        self.0 as i32 == sys::switch_status_t::SWITCH_STATUS_SUCCESS as i32
    }
}

// Bridge from raw FreeSWITCH status values into the safe `Status` newtype. This is the single
// point every internal FFI call site uses to funnel a `sys::switch_status_t` return through
// `status_to_result(impl Into<Status>)`. It is `pub(crate)`-reachable only (the `sys` alias is
// crate-private), so it cannot be invoked from outside `fswtch`. The impl is `#[doc(hidden)]` so
// the `sys::switch_status_t` type does not surface in `Status`'s public rustdoc trait-impl list.
// Removing it would require wrapping ~250 call sites in `Status::from_raw(..)` for no safety
// benefit.
#[doc(hidden)]
impl From<sys::switch_status_t> for Status {
    #[inline]
    fn from(v: sys::switch_status_t) -> Self {
        Self(v)
    }
}

// ── Cause ────────────────────────────────────────────────────────────────

/// FreeSWITCH hangup / origination cause (`switch_call_cause_t`).
///
/// Carried as a `c_uint` newtype (not a Rust `enum`) because the value space is large (82
/// variants) and open: FreeSWITCH may return causes this crate does not enumerate. `from_raw`
/// therefore never fails; use `as_str()` / `is_success()` for inspection.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct Cause(pub(crate) sys::switch_call_cause_t);

impl Cause {
    pub const NONE: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_NONE);
    pub const UNALLOCATED_NUMBER: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_UNALLOCATED_NUMBER);
    pub const NO_ROUTE_TRANSIT_NET: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_NO_ROUTE_TRANSIT_NET);
    pub const NO_ROUTE_DESTINATION: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_NO_ROUTE_DESTINATION);
    pub const CHANNEL_UNACCEPTABLE: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_CHANNEL_UNACCEPTABLE);
    pub const CALL_AWARDED_DELIVERED: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_CALL_AWARDED_DELIVERED);
    pub const NORMAL_CLEARING: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_NORMAL_CLEARING);
    pub const USER_BUSY: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_USER_BUSY);
    pub const NO_USER_RESPONSE: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_NO_USER_RESPONSE);
    pub const NO_ANSWER: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_NO_ANSWER);
    pub const SUBSCRIBER_ABSENT: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_SUBSCRIBER_ABSENT);
    pub const CALL_REJECTED: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_CALL_REJECTED);
    pub const NUMBER_CHANGED: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_NUMBER_CHANGED);
    pub const REDIRECTION_TO_NEW_DESTINATION: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_REDIRECTION_TO_NEW_DESTINATION);
    pub const EXCHANGE_ROUTING_ERROR: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_EXCHANGE_ROUTING_ERROR);
    pub const DESTINATION_OUT_OF_ORDER: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_DESTINATION_OUT_OF_ORDER);
    pub const INVALID_NUMBER_FORMAT: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_INVALID_NUMBER_FORMAT);
    pub const FACILITY_REJECTED: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_FACILITY_REJECTED);
    pub const RESPONSE_TO_STATUS_ENQUIRY: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_RESPONSE_TO_STATUS_ENQUIRY);
    pub const NORMAL_UNSPECIFIED: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_NORMAL_UNSPECIFIED);
    pub const NORMAL_CIRCUIT_CONGESTION: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_NORMAL_CIRCUIT_CONGESTION);
    pub const NETWORK_OUT_OF_ORDER: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_NETWORK_OUT_OF_ORDER);
    pub const NORMAL_TEMPORARY_FAILURE: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_NORMAL_TEMPORARY_FAILURE);
    pub const SWITCH_CONGESTION: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_SWITCH_CONGESTION);
    pub const ACCESS_INFO_DISCARDED: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_ACCESS_INFO_DISCARDED);
    pub const REQUESTED_CHAN_UNAVAIL: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_REQUESTED_CHAN_UNAVAIL);
    pub const PRE_EMPTED: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_PRE_EMPTED);
    pub const FACILITY_NOT_SUBSCRIBED: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_FACILITY_NOT_SUBSCRIBED);
    pub const OUTGOING_CALL_BARRED: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_OUTGOING_CALL_BARRED);
    pub const INCOMING_CALL_BARRED: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_INCOMING_CALL_BARRED);
    pub const BEARERCAPABILITY_NOTAUTH: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_BEARERCAPABILITY_NOTAUTH);
    pub const BEARERCAPABILITY_NOTAVAIL: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_BEARERCAPABILITY_NOTAVAIL);
    pub const SERVICE_UNAVAILABLE: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_SERVICE_UNAVAILABLE);
    pub const BEARERCAPABILITY_NOTIMPL: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_BEARERCAPABILITY_NOTIMPL);
    pub const CHAN_NOT_IMPLEMENTED: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_CHAN_NOT_IMPLEMENTED);
    pub const FACILITY_NOT_IMPLEMENTED: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_FACILITY_NOT_IMPLEMENTED);
    pub const SERVICE_NOT_IMPLEMENTED: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_SERVICE_NOT_IMPLEMENTED);
    pub const INVALID_CALL_REFERENCE: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_INVALID_CALL_REFERENCE);
    pub const INCOMPATIBLE_DESTINATION: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_INCOMPATIBLE_DESTINATION);
    pub const INVALID_IE_CONTENTS: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_INVALID_IE_CONTENTS);
    pub const MANDATORY_IE_MISSING: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_MANDATORY_IE_MISSING);
    pub const MANDATORY_IE_LENGTH_ERROR: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_MANDATORY_IE_LENGTH_ERROR);
    pub const PROTOCOL_ERROR: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_PROTOCOL_ERROR);
    pub const INTERWORKING: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_INTERWORKING);
    pub const ORIGINATOR_CANCEL: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_ORIGINATOR_CANCEL);
    pub const CRASH: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_CRASH);
    pub const SYSTEM_SHUTDOWN: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_SYSTEM_SHUTDOWN);
    pub const LOSE_RACE: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_LOSE_RACE);
    pub const MANAGER_REQUEST: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_MANAGER_REQUEST);
    pub const MEDIA_TIMEOUT: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_MEDIA_TIMEOUT);
    pub const PICKED_OFF: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_PICKED_OFF);
    pub const USER_NOT_REGISTERED: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_USER_NOT_REGISTERED);
    pub const PROGRESS_TIMEOUT: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_PROGRESS_TIMEOUT);
    pub const INVALID_GATEWAY: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_INVALID_GATEWAY);
    pub const GATEWAY_DOWN: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_GATEWAY_DOWN);
    pub const INVALID_URL: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_INVALID_URL);
    pub const INVALID_PROFILE: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_INVALID_PROFILE);
    pub const NO_PICKUP: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_NO_PICKUP);
    pub const SRTP_READ_ERROR: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_SRTP_READ_ERROR);
    pub const BOWOUT: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_BOWOUT);
    pub const ALLOTTED_TIMEOUT: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_ALLOTTED_TIMEOUT);
    pub const RECOVERY_ON_TIMER_EXPIRE: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_RECOVERY_ON_TIMER_EXPIRE);
    pub const INVALID_IDENTITY: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_INVALID_IDENTITY);
    pub const BAD_IDENTITY_INFO: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_BAD_IDENTITY_INFO);
    pub const NO_IDENTITY: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_NO_IDENTITY);
    pub const STALE_DATE: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_STALE_DATE);
    pub const DOES_NOT_EXIST_ANYWHERE: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_DOES_NOT_EXIST_ANYWHERE);
    pub const UNSUPPORTED_CERTIFICATE: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_UNSUPPORTED_CERTIFICATE);
    pub const INVALID_MSG_UNSPECIFIED: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_INVALID_MSG_UNSPECIFIED);
    pub const MESSAGE_TYPE_NONEXIST: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_MESSAGE_TYPE_NONEXIST);
    pub const IE_NONEXIST: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_IE_NONEXIST);
    pub const NOT_ACCEPTABLE: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_NOT_ACCEPTABLE);
    pub const WRONG_CALL_STATE: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_WRONG_CALL_STATE);
    pub const WRONG_MESSAGE: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_WRONG_MESSAGE);
    pub const USER_CHALLENGE: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_USER_CHALLENGE);
    pub const BLIND_TRANSFER: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_BLIND_TRANSFER);
    pub const ATTENDED_TRANSFER: Self =
        Self(sys::switch_call_cause_t_SWITCH_CAUSE_ATTENDED_TRANSFER);
    pub const SUCCESS: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_SUCCESS);
    pub const DECLINE: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_DECLINE);
    pub const BUSY_EVERYWHERE: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_BUSY_EVERYWHERE);
    pub const REJECT_ALL: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_REJECT_ALL);
    pub const UNWANTED: Self = Self(sys::switch_call_cause_t_SWITCH_CAUSE_UNWANTED);

    /// The raw `switch_call_cause_t` value, for FFI.
    #[inline]
    pub(crate) const fn raw(self) -> sys::switch_call_cause_t {
        self.0
    }

    /// Wraps a raw cause returned by FreeSWITCH. Never fails (open value space).
    #[inline]
    pub(crate) const fn from_raw(v: sys::switch_call_cause_t) -> Self {
        Self(v)
    }

    /// `true` when this is `SUCCESS` — the sentinel `switch_core_session_outgoing_channel`
    /// requires to accept a new leg.
    #[inline]
    pub const fn is_success(self) -> bool {
        self.0 == sys::switch_call_cause_t_SWITCH_CAUSE_SUCCESS
    }

    /// FreeSWITCH's string name for this cause (`switch_channel_cause2str`), or `None`.
    pub fn as_str(self) -> Option<&'static str> {
        // SAFETY: `switch_channel_cause2str` is a pure lookup over a static name table.
        let p = unsafe { crate::sys::switch_channel_cause2str(self.0) };
        // SAFETY: `p` is null or a static null-terminated C string; `borrowed_cstr_to_str`
        // copies no data, only re-interprets the pointer's bytes up to the NUL.
        unsafe { crate::command::borrowed_cstr_to_str(p) }
    }
}

// ── ChannelState ─────────────────────────────────────────────────────────

/// FreeSWITCH channel state-machine state (`switch_channel_state_t`).
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct ChannelState(pub(crate) sys::switch_channel_state_t);

impl ChannelState {
    pub const NONE: Self = Self(sys::switch_channel_state_t_CS_NONE);
    pub const NEW: Self = Self(sys::switch_channel_state_t_CS_NEW);
    pub const INIT: Self = Self(sys::switch_channel_state_t_CS_INIT);
    pub const ROUTING: Self = Self(sys::switch_channel_state_t_CS_ROUTING);
    pub const SOFT_EXECUTE: Self = Self(sys::switch_channel_state_t_CS_SOFT_EXECUTE);
    pub const EXECUTE: Self = Self(sys::switch_channel_state_t_CS_EXECUTE);
    pub const EXCHANGE_MEDIA: Self = Self(sys::switch_channel_state_t_CS_EXCHANGE_MEDIA);
    pub const CONSUME_MEDIA: Self = Self(sys::switch_channel_state_t_CS_CONSUME_MEDIA);
    pub const HIBERNATE: Self = Self(sys::switch_channel_state_t_CS_HIBERNATE);
    pub const RESET: Self = Self(sys::switch_channel_state_t_CS_RESET);
    pub const PARK: Self = Self(sys::switch_channel_state_t_CS_PARK);
    pub const REPORTING: Self = Self(sys::switch_channel_state_t_CS_REPORTING);
    pub const HANGUP: Self = Self(sys::switch_channel_state_t_CS_HANGUP);
    pub const DESTROY: Self = Self(sys::switch_channel_state_t_CS_DESTROY);

    /// The raw `switch_channel_state_t` value, for FFI.
    #[inline]
    pub(crate) const fn raw(self) -> sys::switch_channel_state_t {
        self.0
    }

    /// Wraps a raw state returned by FreeSWITCH.
    #[inline]
    pub(crate) const fn from_raw(v: sys::switch_channel_state_t) -> Self {
        Self(v)
    }

    /// `true` for any state at or past `HANGUP` (channel is coming down).
    #[inline]
    pub const fn is_down(self) -> bool {
        self.0 >= sys::switch_channel_state_t_CS_HANGUP
    }
}

// ── CallDirection ────────────────────────────────────────────────────────

/// FreeSWITCH call direction (`switch_call_direction_t`).
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct CallDirection(pub(crate) sys::switch_call_direction_t);

impl CallDirection {
    pub const INBOUND: Self = Self(sys::switch_call_direction_t_SWITCH_CALL_DIRECTION_INBOUND);
    pub const OUTBOUND: Self = Self(sys::switch_call_direction_t_SWITCH_CALL_DIRECTION_OUTBOUND);

    /// The raw `switch_call_direction_t` value, for FFI.
    #[inline]
    pub(crate) const fn raw(self) -> sys::switch_call_direction_t {
        self.0
    }

    /// Wraps a raw direction.
    #[inline]
    pub(crate) const fn from_raw(v: sys::switch_call_direction_t) -> Self {
        Self(v)
    }

    /// `true` when this is `OUTBOUND`.
    #[inline]
    pub const fn is_outbound(self) -> bool {
        self.0 == sys::switch_call_direction_t_SWITCH_CALL_DIRECTION_OUTBOUND
    }
}

// ── OriginateFlag (bitmask) ──────────────────────────────────────────────

/// FreeSWITCH originate flags (`switch_originate_flag_t`). A bitmask — combine with `|`,
/// test with [`contains`](Self::contains).
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct OriginateFlag(pub(crate) sys::switch_originate_flag_t);

impl OriginateFlag {
    pub const NONE: Self = Self(sys::switch_originate_flag_enum_t_SOF_NONE as _);
    pub const NOBLOCK: Self = Self(sys::switch_originate_flag_enum_t_SOF_NOBLOCK as _);
    pub const FORKED_DIAL: Self = Self(sys::switch_originate_flag_enum_t_SOF_FORKED_DIAL as _);
    pub const NO_EFFECTIVE_ANI: Self =
        Self(sys::switch_originate_flag_enum_t_SOF_NO_EFFECTIVE_ANI as _);
    pub const NO_EFFECTIVE_ANIII: Self =
        Self(sys::switch_originate_flag_enum_t_SOF_NO_EFFECTIVE_ANIII as _);
    pub const NO_EFFECTIVE_CID_NUM: Self =
        Self(sys::switch_originate_flag_enum_t_SOF_NO_EFFECTIVE_CID_NUM as _);
    pub const NO_EFFECTIVE_CID_NAME: Self =
        Self(sys::switch_originate_flag_enum_t_SOF_NO_EFFECTIVE_CID_NAME as _);
    pub const NO_LIMITS: Self = Self(sys::switch_originate_flag_enum_t_SOF_NO_LIMITS as _);

    /// The raw bitset value, for FFI.
    #[inline]
    pub(crate) const fn bits(self) -> sys::switch_originate_flag_t {
        self.0
    }

    /// Wraps a raw bitset.
    #[inline]
    pub(crate) const fn from_raw(v: sys::switch_originate_flag_t) -> Self {
        Self(v)
    }

    /// Returns `true` when every bit set in `flag` is also set in `self`. `NONE` contains
    /// only itself.
    #[inline]
    pub const fn contains(self, flag: Self) -> bool {
        if flag.0 == 0 {
            self.0 == 0
        } else {
            (self.0 & flag.0) == flag.0
        }
    }
}

impl std::ops::BitOr for OriginateFlag {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for OriginateFlag {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl std::ops::BitAnd for OriginateFlag {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

impl std::ops::Not for OriginateFlag {
    type Output = Self;
    fn not(self) -> Self {
        Self(!self.0)
    }
}

// ── Status → Result bridge ───────────────────────────────────────────────

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct SwitchError(pub(crate) Status);

impl SwitchError {
    /// Constructs an error carrying `status` (the non-idiomatic-looking constructor for the
    /// private `Status` field; prefer `.into()` from a `Status`).
    #[inline]
    pub const fn new(status: Status) -> Self {
        Self(status)
    }

    /// The FreeSWITCH `Status` carried by this error.
    #[inline]
    pub const fn status(self) -> Status {
        self.0
    }
}

impl From<Status> for SwitchError {
    fn from(status: Status) -> Self {
        Self(status)
    }
}

impl fmt::Display for SwitchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FreeSWITCH returned status {:?}", self.0.0)
    }
}

impl Error for SwitchError {}

pub type Result<T> = std::result::Result<T, SwitchError>;

/// Bridges a FreeSWITCH `Status` to `Result<()>`: `Ok` on `SUCCESS`, else `Err`.
///
/// Accepts anything convertible to `Status` (the newtype itself, or a raw
/// `sys::switch_status_t` returned by an FFI call) so call sites can write
/// `status_to_result(unsafe { sys::foo() })` without an explicit `Status::from_raw`.
pub fn status_to_result(status: impl Into<Status>) -> Result<()> {
    let status = status.into();
    if status.is_success() {
        Ok(())
    } else {
        Err(SwitchError(status))
    }
}

/// Inverts `SUCCESS` → `FALSE`, leaves all other statuses unchanged. Used by helpers that
/// report "did work?" as a `Status` where success means "nothing more to do".
pub fn false_on_success(status: impl Into<Status>) -> Status {
    let status = status.into();
    if status.is_success() {
        Status::FALSE
    } else {
        status
    }
}

// ── Root-level convenience constants (typed newtypes) ────────────────────
//
// These re-export the associated constants at the crate root for brevity in FFI
// glue (e.g. `crate::SUCCESS` instead of `crate::Status::SUCCESS`). Their type is
// the newtype `Status`, so type safety is preserved — they are NOT raw integers.

/// `Status::SUCCESS`, re-exported at the crate root for brevity.
pub const SUCCESS: Status = Status::SUCCESS;
/// `Status::FALSE`, re-exported at the crate root for brevity.
pub const FALSE: Status = Status::FALSE;
/// `Status::GENERR`, re-exported at the crate root for brevity.
pub const GENERR: Status = Status::GENERR;

/// `Cause::SUCCESS`, re-exported at the crate root for brevity.
pub const CAUSE_SUCCESS: Cause = Cause::SUCCESS;
/// `Cause::REQUESTED_CHAN_UNAVAIL`, re-exported at the crate root for brevity.
pub const CAUSE_REQUESTED_CHAN_UNAVAIL: Cause = Cause::REQUESTED_CHAN_UNAVAIL;

/// Maps a Rust `bool` to FreeSWITCH's `switch_bool_t` (`SWITCH_TRUE`/`SWITCH_FALSE`).
pub(crate) fn switch_bool(value: bool) -> sys::switch_bool_t {
    if value {
        sys::switch_bool_t_SWITCH_TRUE
    } else {
        sys::switch_bool_t_SWITCH_FALSE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_roundtrip() {
        assert!(Status::SUCCESS.is_success());
        assert!(!Status::FALSE.is_success());
        assert_eq!(Status::from_raw(Status::GENERR.raw()), Status::GENERR);
    }

    #[test]
    fn status_to_result_maps_success() {
        assert!(status_to_result(Status::SUCCESS).is_ok());
    }

    #[test]
    fn status_to_result_maps_failure() {
        assert!(status_to_result(Status::FALSE).is_err());
        assert!(status_to_result(Status::GENERR).is_err());
    }

    #[test]
    fn false_on_success_inverts_only_success() {
        assert_eq!(false_on_success(Status::SUCCESS), Status::FALSE);
        assert_eq!(false_on_success(Status::FALSE), Status::FALSE);
        assert_eq!(false_on_success(Status::GENERR), Status::GENERR);
    }

    #[test]
    fn cause_success_is_success() {
        assert!(Cause::SUCCESS.is_success());
        assert!(!Cause::NONE.is_success());
    }

    /// Calls `switch_channel_cause2str` (FreeSWITCH FFI), so it only links/runs against a real
    /// FreeSWITCH build — gated behind `live_fs`, like the other FFI-touching unit tests.
    #[cfg(feature = "live_fs")]
    #[test]
    fn cause_as_str_known() {
        // `switch_channel_cause2str` is a static table; these must resolve.
        assert_eq!(Cause::NONE.as_str(), Some("NONE"));
        assert_eq!(Cause::NORMAL_CLEARING.as_str(), Some("NORMAL_CLEARING"));
    }

    #[test]
    fn channel_state_down_predicate() {
        assert!(!ChannelState::CONSUME_MEDIA.is_down());
        assert!(ChannelState::HANGUP.is_down());
        assert!(ChannelState::DESTROY.is_down());
    }

    #[test]
    fn call_direction_outbound() {
        assert!(CallDirection::OUTBOUND.is_outbound());
        assert!(!CallDirection::INBOUND.is_outbound());
    }

    #[test]
    fn originate_flag_combine_and_contains() {
        let f = OriginateFlag::NOBLOCK | OriginateFlag::FORKED_DIAL;
        assert!(f.contains(OriginateFlag::NOBLOCK));
        assert!(f.contains(OriginateFlag::FORKED_DIAL));
        assert!(!f.contains(OriginateFlag::NO_LIMITS));
        // NONE contains only itself.
        assert!(OriginateFlag::NONE.contains(OriginateFlag::NONE));
        assert!(!f.contains(OriginateFlag::NONE));
    }

    #[test]
    fn hup_type_combine_and_contains() {
        let both = HupType::ANSWERED | HupType::UNANSWERED;
        assert!(both.contains(HupType::ANSWERED));
        assert!(both.contains(HupType::UNANSWERED));
        assert!(!both.contains(HupType::NONE));
        // NONE contains only itself.
        assert!(HupType::NONE.contains(HupType::NONE));
        assert_eq!(both.bits(), 3);
    }
}

// ── HupType (bitmask) ────────────────────────────────────────────────────

/// Which legs a batch hangup applies to — a bitmask over `switch_hup_type_t`.
///
/// Used by [`crate::hupall_matching_var`] / [`crate::hupall_matching_vars`]. Combine with `|`:
/// `HupType::ANSWERED | HupType::UNANSWERED` matches both legs (the default of the upstream
/// `switch_core_session_hupall_matching_var` macro, which Rust cannot call since bindgen drops
/// the `#define`).
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct HupType(pub(crate) sys::switch_hup_type_t);

impl HupType {
    pub const NONE: Self = Self(sys::switch_hup_type_t_SHT_NONE);
    pub const UNANSWERED: Self = Self(sys::switch_hup_type_t_SHT_UNANSWERED);
    pub const ANSWERED: Self = Self(sys::switch_hup_type_t_SHT_ANSWERED);

    /// The raw bitset value, for FFI.
    #[inline]
    pub(crate) const fn bits(self) -> sys::switch_hup_type_t {
        self.0
    }

    /// Wraps a raw bitset.
    #[inline]
    #[allow(dead_code)]
    pub(crate) const fn from_raw(v: sys::switch_hup_type_t) -> Self {
        Self(v)
    }

    /// Returns `true` when every bit set in `flag` is also set in `self`. `NONE` contains
    /// only itself.
    #[inline]
    pub const fn contains(self, flag: Self) -> bool {
        if flag.0 == 0 {
            self.0 == 0
        } else {
            (self.0 & flag.0) == flag.0
        }
    }
}

impl std::ops::BitOr for HupType {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for HupType {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl std::ops::BitAnd for HupType {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

impl std::ops::Not for HupType {
    type Output = Self;
    fn not(self) -> Self {
        Self(!self.0)
    }
}
