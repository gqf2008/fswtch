use crate::{
    Cause, Pool, Result, Session, borrowed_cstr_to_str, cstring, status_to_result,
    strdup_to_string, sys,
};

use std::ffi::c_char;
use std::mem::MaybeUninit;
use std::ptr::NonNull;

/// FreeSWITCH media flag bitset (`switch_media_flag_t`), passed to [`media`] / [`nomedia`].
///
/// A newtype over the raw `u32` bitset so callers cannot mix it with other flag types. The
/// underlying enum is `switch_media_flag_enum_t` (`SMF_*`); every variant is a power of two (or
/// `0` for [`MediaFlag::NONE`]), so this is a bitmask — combine with `|`, test with
/// [`contains`](Self::contains), matching the [`crate::ChannelFlag`] / [`crate::IoFlags`] pattern.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct MediaFlag(pub(crate) sys::switch_media_flag_t);

impl MediaFlag {
    pub const NONE: Self = Self(sys::switch_media_flag_enum_t_SMF_NONE);
    pub const REBRIDGE: Self = Self(sys::switch_media_flag_enum_t_SMF_REBRIDGE);
    pub const ECHO_ALEG: Self = Self(sys::switch_media_flag_enum_t_SMF_ECHO_ALEG);
    pub const ECHO_BLEG: Self = Self(sys::switch_media_flag_enum_t_SMF_ECHO_BLEG);
    pub const FORCE: Self = Self(sys::switch_media_flag_enum_t_SMF_FORCE);
    pub const LOOP: Self = Self(sys::switch_media_flag_enum_t_SMF_LOOP);
    pub const HOLD_BLEG: Self = Self(sys::switch_media_flag_enum_t_SMF_HOLD_BLEG);
    pub const IMMEDIATE: Self = Self(sys::switch_media_flag_enum_t_SMF_IMMEDIATE);
    pub const EXEC_INLINE: Self = Self(sys::switch_media_flag_enum_t_SMF_EXEC_INLINE);
    pub const PRIORITY: Self = Self(sys::switch_media_flag_enum_t_SMF_PRIORITY);
    pub const REPLYONLY_A: Self = Self(sys::switch_media_flag_enum_t_SMF_REPLYONLY_A);
    pub const REPLYONLY_B: Self = Self(sys::switch_media_flag_enum_t_SMF_REPLYONLY_B);

    /// The raw bitset value, for FFI.
    #[inline]
    pub(crate) const fn bits(self) -> sys::switch_media_flag_t {
        self.0
    }

    /// Wraps a raw `switch_media_flag_t` (e.g. an `SMF_*` constant or an OR-ed combination).
    #[inline]
    #[allow(dead_code)]
    pub(crate) const fn from_raw(v: sys::switch_media_flag_t) -> Self {
        Self(v)
    }

    /// Returns `true` when every bit set in `flag` is also set in `self`. `NONE` (=0) contains only
    /// itself.
    #[inline]
    pub const fn contains(self, flag: Self) -> bool {
        if flag.0 == 0 {
            self.0 == 0
        } else {
            (self.0 & flag.0) == flag.0
        }
    }
}

impl std::ops::BitOr for MediaFlag {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for MediaFlag {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

// ── DigitActionTarget ────────────────────────────────────────────────────

/// The leg a [`DigitMachine`]'s matched actions fire against
/// (`switch_digit_action_target_t`). A single-valued enum — pass to
/// [`DigitMachine::set_target`] and read back from [`DigitMachine::get_target`].
///
/// `SELF` applies the action to the session that owns the machine; `PEER` to its bridged
/// partner; `BOTH` to both legs.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct DigitActionTarget(pub(crate) sys::switch_digit_action_target_t);

impl DigitActionTarget {
    /// Apply matched actions to the owning session's own leg.
    pub const SELF: Self = Self(sys::switch_digit_action_target_t_DIGIT_TARGET_SELF);
    /// Apply matched actions to the bridged peer leg.
    pub const PEER: Self = Self(sys::switch_digit_action_target_t_DIGIT_TARGET_PEER);
    /// Apply matched actions to both legs.
    pub const BOTH: Self = Self(sys::switch_digit_action_target_t_DIGIT_TARGET_BOTH);

    /// The raw `switch_digit_action_target_t` value, for FFI.
    #[inline]
    pub(crate) const fn raw(self) -> sys::switch_digit_action_target_t {
        self.0
    }

    /// Wraps a raw target returned by FreeSWITCH.
    #[inline]
    pub(crate) const fn from_raw(v: sys::switch_digit_action_target_t) -> Self {
        Self(v)
    }

    /// `true` when actions fire against the owning session's own leg (`SELF` or `BOTH`).
    #[inline]
    pub const fn is_self(self) -> bool {
        self.0 == sys::switch_digit_action_target_t_DIGIT_TARGET_SELF
            || self.0 == sys::switch_digit_action_target_t_DIGIT_TARGET_BOTH
    }

    /// `true` when actions fire against the bridged peer leg (`PEER` or `BOTH`).
    #[inline]
    pub const fn is_peer(self) -> bool {
        self.0 == sys::switch_digit_action_target_t_DIGIT_TARGET_PEER
            || self.0 == sys::switch_digit_action_target_t_DIGIT_TARGET_BOTH
    }
}

impl From<sys::switch_digit_action_target_t> for DigitActionTarget {
    fn from(v: sys::switch_digit_action_target_t) -> Self {
        Self(v)
    }
}

/// Broadcasts `path` (a media path or app/arg string) to the channel identified by `uuid`, applying
/// `flags` (a FreeSWITCH `switch_media_flag_t` bitset; pass `0` for the default).
///
/// Unlike most functions in this module this takes a UUID rather than a [`Session`], matching the
/// underlying C signature.
pub fn broadcast(uuid: impl AsRef<str>, path: impl AsRef<str>, flags: u32) -> Result<()> {
    let uuid = cstring(uuid)?;
    let path = cstring(path)?;
    // SAFETY: `uuid` and `path` are valid C strings for the duration of the call.
    let status = unsafe {
        sys::switch_ivr_broadcast(
            uuid.as_ptr(),
            path.as_ptr(),
            flags as sys::switch_media_flag_t,
        )
    };
    status_to_result(status)
}

/// Looks up the presence mapping for `exten_name` in `domain_name` and returns the resolved
/// presence ID as an owned string, or `Ok(None)` when no mapping exists. The C function returns a
/// `malloc`-allocated string that [`strdup_to_string`](crate::strdup_to_string) copies and frees.
pub fn check_presence_mapping(
    exten_name: impl AsRef<str>,
    domain_name: impl AsRef<str>,
) -> Result<Option<String>> {
    let exten = cstring(exten_name)?;
    let domain = cstring(domain_name)?;
    // SAFETY: `exten` and `domain` are valid C strings for the duration of the call; the returned
    // pointer is either null or a `malloc`-allocated C string that `strdup_to_string` owns.
    let raw = unsafe { sys::switch_ivr_check_presence_mapping(exten.as_ptr(), domain.as_ptr()) };
    // SAFETY: `raw` is null or a malloc'd C string per the function's contract.
    Ok(unsafe { strdup_to_string(raw.cast()) })
}

// ---------------------------------------------------------------------------
// Originate / bridge
// ---------------------------------------------------------------------------

/// Outcome of [`Session::originate`] / [`Session::originate_raw`]: the (possibly absent) created B-leg session, the
/// disconnect cause FreeSWITCH assigned to the attempt, and the cancel cause (set when the
/// originate was aborted before completing).
pub struct OriginateOutcome {
    /// The newly created outbound session, when the call was answered. The caller owns the use of
    /// this handle for the originate bridge window; FreeSWITCH retains the underlying session.
    pub peer: Option<Session>,
    /// `SWITCH_CAUSE_*` value describing why the originate ended (e.g. `CAUSE_NORMAL_CLEARING` on
    /// success, `CAUSE_NO_ANSWER` / `CAUSE_USER_BUSY` / `CAUSE_NO_USER_RESPONSE` on failure).
    pub cause: Cause,
    /// Cancel cause, populated when the originate was cancelled (e.g. `CAUSE_ORIGINATOR_CANCEL`).
    pub cancel_cause: Cause,
}

impl std::fmt::Debug for OriginateOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OriginateOutcome")
            .field("peer", &self.peer.as_ref().map(|s| s.as_ptr()))
            .field("cause", &self.cause)
            .field("cancel_cause", &self.cancel_cause)
            .finish()
    }
}

/// Takes media (pushes the named session into the `CS_CONSUME_MEDIA`/early-media path) on the
/// channel identified by `uuid`. `flags` is a bitmask of `SMF_*` media flags — combine
/// [`MediaFlag`] variants with `|` (e.g. `MediaFlag::FORCE | MediaFlag::REBRIDGE`), or pass
/// [`MediaFlag::NONE`] for the default.
pub fn media(uuid: impl AsRef<str>, flags: MediaFlag) -> Result<()> {
    let uuid = cstring(uuid)?;
    // SAFETY: `uuid` is a valid C string for the call; `flags.bits()` is the raw `switch_media_flag_t`.
    let status = unsafe { sys::switch_ivr_media(uuid.as_ptr(), flags.bits()) };
    status_to_result(status)
}

/// Drops media on the channel identified by `uuid` (the inverse of [`media`]). `flags` is a bitmask
/// of `SMF_*` media flags — combine [`MediaFlag`] variants with `|`, or pass
/// [`MediaFlag::NONE`] for the default.
pub fn nomedia(uuid: impl AsRef<str>, flags: MediaFlag) -> Result<()> {
    let uuid = cstring(uuid)?;
    // SAFETY: `uuid` is a valid C string for the call; `flags.bits()` is the raw `switch_media_flag_t`.
    let status = unsafe { sys::switch_ivr_nomedia(uuid.as_ptr(), flags.bits()) };
    status_to_result(status)
}

// ---------------------------------------------------------------------------
// Say (pronunciation)
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// UUID presence
// ---------------------------------------------------------------------------

/// Returns `true` when a channel with the given `uuid` currently exists in the core.
pub fn uuid_exists(uuid: impl AsRef<str>) -> Result<bool> {
    let uuid = cstring(uuid)?;
    // SAFETY: `uuid` is a valid C string for the call.
    let r = unsafe { sys::switch_ivr_uuid_exists(uuid.as_ptr()) };
    Ok(r != sys::switch_bool_t_SWITCH_FALSE)
}

/// Forces the core to consider a channel with the given `uuid` as existing (used to keep a
/// locate-by-uuid lookup alive across a transfer). Returns the previous existence state.
pub fn uuid_force_exists(uuid: impl AsRef<str>) -> Result<bool> {
    let uuid = cstring(uuid)?;
    // SAFETY: `uuid` is a valid C string for the call.
    let r = unsafe { sys::switch_ivr_uuid_force_exists(uuid.as_ptr()) };
    Ok(r != sys::switch_bool_t_SWITCH_FALSE)
}

// ---------------------------------------------------------------------------
// Event parsing
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// IVR menu (switch_ivr_menu_*) — owned wrapper over switch_ivr_menu_t
// ---------------------------------------------------------------------------

/// Configuration for [`IvrMenu::new`] / [`IvrMenu::new_sub`]. Sound/macro fields accept `&str`
/// (copied into C strings for the call); pass empty strings where FreeSWITCH accepts null.
#[derive(Debug)]
pub struct IvrMenuConfig<'a> {
    /// Menu name (used by [`IvrMenu::execute`] lookup).
    pub name: &'a str,
    /// Sound played on entry.
    pub greeting_sound: &'a str,
    /// Short greeting (played on repeat visits).
    pub short_greeting_sound: &'a str,
    /// Sound played on invalid input.
    pub invalid_sound: &'a str,
    /// Sound played on exit.
    pub exit_sound: &'a str,
    /// Sound played on transfer.
    pub transfer_sound: &'a str,
    /// Confirmation macro (empty for none).
    pub confirm_macro: &'a str,
    /// Confirmation key (empty for none).
    pub confirm_key: &'a str,
    /// TTS engine (empty for none).
    pub tts_engine: &'a str,
    /// TTS voice (empty for none).
    pub tts_voice: &'a str,
    /// Number of confirmation attempts.
    pub confirm_attempts: i32,
    /// Inter-digit timeout (ms).
    pub inter_timeout: i32,
    /// Expected digit length.
    pub digit_len: i32,
    /// Overall menu timeout (ms).
    pub timeout: i32,
    /// Max consecutive failures before exit.
    pub max_failures: i32,
    /// Max consecutive timeouts before exit.
    pub max_timeouts: i32,
}

impl<'a> IvrMenuConfig<'a> {
    /// A minimal config: only `name` is set; every sound/macro field is empty and every timing
    /// field is zero. Suitable for menus driven entirely by bound actions.
    pub fn minimal(name: &'a str) -> Self {
        Self {
            name,
            greeting_sound: "",
            short_greeting_sound: "",
            invalid_sound: "",
            exit_sound: "",
            transfer_sound: "",
            confirm_macro: "",
            confirm_key: "",
            tts_engine: "",
            tts_voice: "",
            confirm_attempts: 0,
            inter_timeout: 0,
            digit_len: 0,
            timeout: 0,
            max_failures: 0,
            max_timeouts: 0,
        }
    }
}

/// An owned IVR menu (`switch_ivr_menu_t`).
///
/// Created from a [`Pool`] via [`IvrMenu::new`] (which calls `switch_ivr_menu_init`). The menu may
/// be a sub-menu of another `IvrMenu` (pass the parent to [`IvrMenu::new_sub`]); the parent must
/// outlive the child. On drop the menu stack is freed with `switch_ivr_menu_stack_free`.
pub struct IvrMenu {
    raw: NonNull<sys::switch_ivr_menu_t>,
}

impl IvrMenu {
    /// Creates a new top-level IVR menu from `config`, allocating against `pool`.
    pub fn new(pool: &Pool, config: &IvrMenuConfig<'_>) -> Result<Self> {
        Self::build(std::ptr::null_mut(), pool, config)
    }

    /// Creates a sub-menu of `parent`, inheriting its pool-backed storage. `parent` must outlive
    /// the returned menu.
    pub fn new_sub(parent: &IvrMenu, pool: &Pool, config: &IvrMenuConfig<'_>) -> Result<Self> {
        Self::build(parent.raw.as_ptr(), pool, config)
    }

    fn build(
        main: *mut sys::switch_ivr_menu_t,
        pool: &Pool,
        config: &IvrMenuConfig<'_>,
    ) -> Result<Self> {
        let name = cstring(config.name)?;
        let greeting = cstring(config.greeting_sound)?;
        let short_greeting = cstring(config.short_greeting_sound)?;
        let invalid = cstring(config.invalid_sound)?;
        let exit = cstring(config.exit_sound)?;
        let transfer = cstring(config.transfer_sound)?;
        let confirm_macro = cstring(config.confirm_macro)?;
        let confirm_key = cstring(config.confirm_key)?;
        let tts_engine = cstring(config.tts_engine)?;
        let tts_voice = cstring(config.tts_voice)?;
        let mut new_menu: *mut sys::switch_ivr_menu_t = std::ptr::null_mut();
        // SAFETY: `pool.as_ptr()` is a live pool; `main` is null (top-level) or a valid parent menu
        // pointer; every string pointer is a valid C string for the call; `new_menu` is a valid
        // out-pointer.
        let status = unsafe {
            sys::switch_ivr_menu_init(
                &mut new_menu,
                main,
                name.as_ptr(),
                greeting.as_ptr(),
                short_greeting.as_ptr(),
                invalid.as_ptr(),
                exit.as_ptr(),
                transfer.as_ptr(),
                confirm_macro.as_ptr(),
                confirm_key.as_ptr(),
                tts_engine.as_ptr(),
                tts_voice.as_ptr(),
                config.confirm_attempts,
                config.inter_timeout,
                config.digit_len,
                config.timeout,
                config.max_failures,
                config.max_timeouts,
                pool.as_ptr(),
            )
        };
        status_to_result(status)?;
        // SAFETY: `switch_ivr_menu_init` returned SUCCESS, so `new_menu` is non-null.
        let raw = NonNull::new(new_menu).ok_or(crate::SwitchError(crate::GENERR))?;
        Ok(Self { raw })
    }

    /// The underlying `switch_ivr_menu_t *`. Valid for as long as this `IvrMenu` is alive.
    #[inline]
    pub fn as_ptr(&self) -> *mut sys::switch_ivr_menu_t {
        self.raw.as_ptr()
    }

    /// Binds `digits` (a DTMF pattern) to a built-in `SWITCH_IVR_ACTION_*` action with an `arg`
    /// string (e.g. a sound path for `SWITCH_IVR_ACTION_PLAYSOUND` or an app string for
    /// `SWITCH_IVR_ACTION_EXECAPP`).
    pub fn bind_action(
        &self,
        ivr_action: sys::switch_ivr_action_t,
        arg: impl AsRef<str>,
        digits: impl AsRef<str>,
    ) -> Result<()> {
        let arg = cstring(arg)?;
        let digits = cstring(digits)?;
        // SAFETY: `self.raw` is a live menu; both strings are valid C strings for the call.
        let status = unsafe {
            sys::switch_ivr_menu_bind_action(
                self.raw.as_ptr(),
                ivr_action,
                arg.as_ptr(),
                digits.as_ptr(),
            )
        };
        status_to_result(status)
    }

    /// Binds `digits` to a custom `function` (an `unsafe extern "C"` menu-action callback), passing
    /// `arg` as its bound argument. The callback must remain valid for the menu's lifetime.
    pub fn bind_function(
        &self,
        function: sys::switch_ivr_menu_action_function_t,
        arg: impl AsRef<str>,
        digits: impl AsRef<str>,
    ) -> Result<()> {
        let arg = cstring(arg)?;
        let digits = cstring(digits)?;
        // SAFETY: `self.raw` is a live menu; `function` is a valid function pointer; both strings
        // are valid C strings for the call.
        let status = unsafe {
            sys::switch_ivr_menu_bind_function(
                self.raw.as_ptr(),
                function,
                arg.as_ptr(),
                digits.as_ptr(),
            )
        };
        status_to_result(status)
    }

    /// Executes the menu against `session` under `name` (the menu stack lookup key), passing `obj`
    /// through to bound action callbacks. `obj` is opaque to FreeSWITCH and must remain valid for
    /// the call.
    pub fn execute(
        &self,
        session: Session,
        name: impl AsRef<str>,
        obj: *mut std::ffi::c_void,
    ) -> Result<()> {
        let name = cstring(name)?;
        // SAFETY: `session.as_ptr()` is live; `self.raw` is a live menu; `name` is a valid C string
        // for the call; `obj` is opaque and valid per the caller's contract.
        let status = unsafe {
            sys::switch_ivr_menu_execute(
                session.as_ptr(),
                self.raw.as_ptr(),
                name.as_ptr() as *mut c_char,
                obj,
            )
        };
        status_to_result(status)
    }
}

impl Drop for IvrMenu {
    fn drop(&mut self) {
        // SAFETY: `self.raw` was created by `switch_ivr_menu_init` and has not been freed yet;
        // `switch_ivr_menu_stack_free` tears down the menu stack and is safe to call once.
        let _ = unsafe { sys::switch_ivr_menu_stack_free(self.raw.as_ptr()) };
    }
}

// ---------------------------------------------------------------------------
// Digit machine (switch_ivr_dmachine_*) — owned wrapper over switch_ivr_dmachine_t
// ---------------------------------------------------------------------------

/// An owned digit-collection state machine (`switch_ivr_dmachine_t`).
///
/// Created from a [`Pool`] via [`DigitMachine::new`] (which calls `switch_ivr_dmachine_create`).
/// On drop the machine is destroyed with `switch_ivr_dmachine_destroy`.
pub struct DigitMachine {
    raw: NonNull<sys::switch_ivr_dmachine_t>,
}

/// A live view of a digit-machine match, borrowed from the machine. Drop this before mutating the
/// machine again.
#[derive(Copy, Clone)]
pub struct DmachineMatch<'a> {
    raw: *mut sys::switch_ivr_dmachine_match_t,
    _life: std::marker::PhantomData<&'a sys::switch_ivr_dmachine_match_t>,
}

impl<'a> DmachineMatch<'a> {
    /// The matched digit string, borrowed from machine storage (valid for the match's lifetime).
    pub fn match_digits(&self) -> Option<&str> {
        // SAFETY: `self.raw` is a live match struct; `match_digits` is null or a valid C string
        // borrowed from the machine for the match's lifetime, which is bounded by `'a`.
        unsafe { borrowed_cstr_to_str((*self.raw).match_digits) }
    }

    /// The caller-supplied key for the bound pattern that matched.
    pub fn match_key(&self) -> i32 {
        // SAFETY: `self.raw` is a live match struct.
        unsafe { (*self.raw).match_key }
    }

    /// Whether this was a positive (`DM_MATCH_POSITIVE`) or negative (`DM_MATCH_NEGATIVE`) match.
    pub fn match_type(&self) -> sys::dm_match_type_t {
        // SAFETY: `self.raw` is a live match struct.
        unsafe { (*self.raw).type_ }
    }
}

impl DigitMachine {
    /// Creates a new digit machine named `name`, allocating against `pool`, with the given
    /// `digit_timeout` and `input_timeout` (ms). The match/non-match callbacks may be `None`.
    /// `user_data` is opaque to FreeSWITCH and must remain valid for the machine's lifetime.
    pub fn new(
        name: impl AsRef<str>,
        pool: &Pool,
        digit_timeout: u32,
        input_timeout: u32,
        match_callback: sys::switch_ivr_dmachine_callback_t,
        nonmatch_callback: sys::switch_ivr_dmachine_callback_t,
        user_data: *mut std::ffi::c_void,
    ) -> Result<Self> {
        let name = cstring(name)?;
        let mut dm: *mut sys::switch_ivr_dmachine_t = std::ptr::null_mut();
        // SAFETY: `pool.as_ptr()` is a live pool; `name` is a valid C string for the call; `dm` is
        // a valid out-pointer; the callbacks (when provided) are valid function pointers; `user_data`
        // is opaque and valid per the caller's contract.
        let status = unsafe {
            sys::switch_ivr_dmachine_create(
                &mut dm,
                name.as_ptr(),
                pool.as_ptr(),
                digit_timeout,
                input_timeout,
                match_callback,
                nonmatch_callback,
                user_data,
            )
        };
        status_to_result(status)?;
        // SAFETY: `switch_ivr_dmachine_create` returned SUCCESS, so `dm` is non-null.
        let raw = NonNull::new(dm).ok_or(crate::SwitchError(crate::GENERR))?;
        Ok(Self { raw })
    }

    /// The underlying `switch_ivr_dmachine_t *`. Valid for as long as this `DigitMachine` is alive.
    #[inline]
    pub fn as_ptr(&self) -> *mut sys::switch_ivr_dmachine_t {
        self.raw.as_ptr()
    }

    /// Binds `digits` (a pattern, may contain `.`/`|`/`*`/`#`) in `realm` to `key`, optionally
    /// marking it priority (`is_priority`) and attaching a per-match `callback` and `user_data`.
    pub fn bind(
        &self,
        realm: impl AsRef<str>,
        digits: impl AsRef<str>,
        is_priority: bool,
        key: i32,
        callback: sys::switch_ivr_dmachine_callback_t,
        user_data: *mut std::ffi::c_void,
    ) -> Result<()> {
        let realm = cstring(realm)?;
        let digits = cstring(digits)?;
        // SAFETY: `self.raw` is a live machine; both strings are valid C strings for the call; the
        // callback (when provided) is a valid function pointer; `user_data` is opaque and valid per
        // the caller's contract.
        let status = unsafe {
            sys::switch_ivr_dmachine_bind(
                self.raw.as_ptr(),
                realm.as_ptr(),
                digits.as_ptr(),
                if is_priority { 1 } else { 0 },
                key,
                callback,
                user_data,
            )
        };
        status_to_result(status)
    }

    /// Feeds `digits` into the machine. On a terminal match the match is returned via the borrow;
    /// pass `None` to ignore the produced match (it remains queryable via [`Self::get_match`]).
    pub fn feed<'a>(&'a self, digits: impl AsRef<str>) -> Result<Option<DmachineMatch<'a>>> {
        let digits = cstring(digits)?;
        let mut m: *mut sys::switch_ivr_dmachine_match_t = std::ptr::null_mut();
        // SAFETY: `self.raw` is a live machine; `digits` is a valid C string for the call; `m` is a
        // valid out-pointer. When non-null it points at machine-owned storage that outlives `&self`.
        let status =
            unsafe { sys::switch_ivr_dmachine_feed(self.raw.as_ptr(), digits.as_ptr(), &mut m) };
        status_to_result(status)?;
        if m.is_null() {
            return Ok(None);
        }
        Ok(Some(DmachineMatch {
            raw: m,
            _life: std::marker::PhantomData,
        }))
    }

    /// Pings the machine for a pending match without feeding new digits. The returned match, if
    /// any, borrows machine storage.
    pub fn ping<'a>(&'a self) -> Result<Option<DmachineMatch<'a>>> {
        let mut m: *mut sys::switch_ivr_dmachine_match_t = std::ptr::null_mut();
        // SAFETY: `self.raw` is a live machine; `m` is a valid out-pointer.
        let status = unsafe { sys::switch_ivr_dmachine_ping(self.raw.as_ptr(), &mut m) };
        status_to_result(status)?;
        if m.is_null() {
            return Ok(None);
        }
        Ok(Some(DmachineMatch {
            raw: m,
            _life: std::marker::PhantomData,
        }))
    }

    /// Returns the most recent match, borrowed from machine storage, or `None` when there is none.
    pub fn get_match<'a>(&'a self) -> Option<DmachineMatch<'a>> {
        // SAFETY: `self.raw` is a live machine; the returned pointer is null or points at
        // machine-owned storage that outlives `&self`.
        let m = unsafe { sys::switch_ivr_dmachine_get_match(self.raw.as_ptr()) };
        if m.is_null() {
            return None;
        }
        Some(DmachineMatch {
            raw: m,
            _life: std::marker::PhantomData,
        })
    }

    /// The digits that failed to match (borrowed from machine storage), if any.
    pub fn failed_digits(&self) -> Option<&str> {
        // SAFETY: `self.raw` is a live machine; the returned pointer is null or a valid C string
        // borrowed from machine storage for the lifetime of `&self`.
        let p = unsafe { sys::switch_ivr_dmachine_get_failed_digits(self.raw.as_ptr()) };
        // SAFETY: forwarded from the contract above.
        unsafe { borrowed_cstr_to_str(p) }
    }

    /// The machine name (borrowed from machine storage).
    pub fn name(&self) -> Option<&str> {
        // SAFETY: `self.raw` is a live machine; the returned pointer is null or a valid C string
        // borrowed from machine storage for the lifetime of `&self`.
        let p = unsafe { sys::switch_ivr_dmachine_get_name(self.raw.as_ptr()) };
        // SAFETY: forwarded from the contract above.
        unsafe { borrowed_cstr_to_str(p) }
    }

    /// Clears pending digits in the machine.
    pub fn clear(&self) -> Result<()> {
        // SAFETY: `self.raw` is a live machine.
        let status = unsafe { sys::switch_ivr_dmachine_clear(self.raw.as_ptr()) };
        status_to_result(status)
    }

    /// Sets the active `realm` (determines which bound patterns are eligible to match).
    pub fn set_realm(&self, realm: impl AsRef<str>) -> Result<()> {
        let realm = cstring(realm)?;
        // SAFETY: `self.raw` is a live machine; `realm` is a valid C string for the call.
        let status =
            unsafe { sys::switch_ivr_dmachine_set_realm(self.raw.as_ptr(), realm.as_ptr()) };
        status_to_result(status)
    }

    /// Clears all bindings in `realm`.
    pub fn clear_realm(&self, realm: impl AsRef<str>) -> Result<()> {
        let realm = cstring(realm)?;
        // SAFETY: `self.raw` is a live machine; `realm` is a valid C string for the call.
        let status =
            unsafe { sys::switch_ivr_dmachine_clear_realm(self.raw.as_ptr(), realm.as_ptr()) };
        status_to_result(status)
    }

    /// Sets the digit (inter-digit) timeout, in milliseconds.
    pub fn set_digit_timeout_ms(&self, digit_timeout_ms: u32) {
        // SAFETY: `self.raw` is a live machine.
        unsafe {
            sys::switch_ivr_dmachine_set_digit_timeout_ms(self.raw.as_ptr(), digit_timeout_ms)
        };
    }

    /// Sets the overall input timeout, in milliseconds.
    pub fn set_input_timeout_ms(&self, input_timeout_ms: u32) {
        // SAFETY: `self.raw` is a live machine.
        unsafe {
            sys::switch_ivr_dmachine_set_input_timeout_ms(self.raw.as_ptr(), input_timeout_ms)
        };
    }

    /// Sets the DTMF terminator characters (e.g. `"#"`).
    pub fn set_terminators(&self, terminators: impl AsRef<str>) -> Result<()> {
        let terminators = cstring(terminators)?;
        // SAFETY: `self.raw` is a live machine; `terminators` is a valid C string for the call.
        let status = unsafe {
            sys::switch_ivr_dmachine_set_terminators(self.raw.as_ptr(), terminators.as_ptr())
        };
        status_to_result(status)
    }

    /// Sets the digit-action target ([`DigitActionTarget::SELF`] / [`DigitActionTarget::PEER`] /
    /// [`DigitActionTarget::BOTH`]).
    pub fn set_target(&self, target: DigitActionTarget) {
        // SAFETY: `self.raw` is a live machine.
        unsafe { sys::switch_ivr_dmachine_set_target(self.raw.as_ptr(), target.raw()) };
    }

    /// The current digit-action target.
    pub fn get_target(&self) -> DigitActionTarget {
        // SAFETY: `self.raw` is a live machine.
        let raw = unsafe { sys::switch_ivr_dmachine_get_target(self.raw.as_ptr()) };
        DigitActionTarget::from_raw(raw)
    }

    /// Whether the machine is currently mid-parse (has buffered digits awaiting a terminator).
    pub fn is_parsing(&self) -> bool {
        // SAFETY: `self.raw` is a live machine.
        let r = unsafe { sys::switch_ivr_dmachine_is_parsing(self.raw.as_ptr()) };
        r != sys::switch_bool_t_SWITCH_FALSE
    }
}

impl Drop for DigitMachine {
    fn drop(&mut self) {
        // SAFETY: `self.raw` was created by `switch_ivr_dmachine_create` and has not been destroyed
        // yet; `switch_ivr_dmachine_destroy` nulls the out-pointer and is safe to call once.
        let mut raw = self.raw.as_ptr();
        unsafe { sys::switch_ivr_dmachine_destroy(&mut raw) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_flag_raw_roundtrip() {
        // from_raw/bits is a pure newtype pass-through, so every SMF_* constant survives a round
        // trip and equals its variant.
        assert_eq!(
            MediaFlag::NONE.bits(),
            sys::switch_media_flag_enum_t_SMF_NONE
        );
        assert_eq!(
            MediaFlag::FORCE.bits(),
            sys::switch_media_flag_enum_t_SMF_FORCE
        );
        assert_eq!(
            MediaFlag::from_raw(sys::switch_media_flag_enum_t_SMF_PRIORITY).bits(),
            MediaFlag::PRIORITY.bits()
        );
        assert_eq!(
            MediaFlag::from_raw(sys::switch_media_flag_enum_t_SMF_IMMEDIATE),
            MediaFlag::IMMEDIATE
        );
    }

    #[test]
    fn media_flag_combine_and_contains() {
        let f = MediaFlag::FORCE | MediaFlag::REBRIDGE;
        assert!(f.contains(MediaFlag::FORCE));
        assert!(f.contains(MediaFlag::REBRIDGE));
        assert!(!f.contains(MediaFlag::LOOP));
        // NONE contains only itself; a non-NONE bitset does not contain NONE.
        assert!(MediaFlag::NONE.contains(MediaFlag::NONE));
        assert!(!f.contains(MediaFlag::NONE));
    }

    #[test]
    fn media_flag_bitor_assign() {
        let mut f = MediaFlag::ECHO_ALEG;
        f |= MediaFlag::ECHO_BLEG;
        assert_eq!(
            f.bits(),
            sys::switch_media_flag_enum_t_SMF_ECHO_ALEG
                | sys::switch_media_flag_enum_t_SMF_ECHO_BLEG
        );
        assert!(f.contains(MediaFlag::ECHO_ALEG));
        assert!(f.contains(MediaFlag::ECHO_BLEG));
    }

    #[test]
    fn digit_action_target_raw_roundtrip() {
        // from_raw/raw is a pure newtype pass-through, so each DIGIT_TARGET_* constant survives a
        // round trip and equals its variant.
        assert_eq!(
            DigitActionTarget::SELF.raw(),
            sys::switch_digit_action_target_t_DIGIT_TARGET_SELF
        );
        assert_eq!(
            DigitActionTarget::PEER.raw(),
            sys::switch_digit_action_target_t_DIGIT_TARGET_PEER
        );
        assert_eq!(
            DigitActionTarget::BOTH.raw(),
            sys::switch_digit_action_target_t_DIGIT_TARGET_BOTH
        );
        assert_eq!(
            DigitActionTarget::from_raw(sys::switch_digit_action_target_t_DIGIT_TARGET_PEER),
            DigitActionTarget::PEER
        );
        assert_eq!(
            DigitActionTarget::from_raw(sys::switch_digit_action_target_t_DIGIT_TARGET_BOTH).raw(),
            DigitActionTarget::BOTH.raw()
        );
    }

    #[test]
    fn digit_action_target_predicates() {
        // SELF targets only the owning leg; PEER only the bridged leg; BOTH targets both.
        assert!(DigitActionTarget::SELF.is_self());
        assert!(!DigitActionTarget::SELF.is_peer());

        assert!(DigitActionTarget::PEER.is_peer());
        assert!(!DigitActionTarget::PEER.is_self());

        assert!(DigitActionTarget::BOTH.is_self());
        assert!(DigitActionTarget::BOTH.is_peer());
    }
}

// ── TTS / hold / transfer / bridge / kill (high-frequency) ────────────────

/// Holds the session identified by `uuid`.
pub fn hold_uuid(uuid: impl AsRef<str>, message: impl AsRef<str>, moh: bool) -> Result<()> {
    let uuid = cstring(uuid)?;
    let message = cstring(message)?;
    let moh = if moh {
        sys::switch_bool_t_SWITCH_TRUE
    } else {
        sys::switch_bool_t_SWITCH_FALSE
    };
    // SAFETY: valid uuid C string; valid message; valid bool.
    status_to_result(unsafe { sys::switch_ivr_hold_uuid(uuid.as_ptr(), message.as_ptr(), moh) })
}

/// Toggles hold on the session identified by `uuid`.
pub fn hold_toggle_uuid(uuid: impl AsRef<str>, message: impl AsRef<str>, moh: bool) -> Result<()> {
    let uuid = cstring(uuid)?;
    let message = cstring(message)?;
    let moh = if moh {
        sys::switch_bool_t_SWITCH_TRUE
    } else {
        sys::switch_bool_t_SWITCH_FALSE
    };
    // SAFETY: valid uuid; valid message; valid bool.
    status_to_result(unsafe {
        sys::switch_ivr_hold_toggle_uuid(uuid.as_ptr(), message.as_ptr(), moh)
    })
}

/// Unholds the session identified by `uuid`.
pub fn unhold_uuid(uuid: impl AsRef<str>) -> Result<()> {
    let uuid = cstring(uuid)?;
    // SAFETY: valid uuid.
    status_to_result(unsafe { sys::switch_ivr_unhold_uuid(uuid.as_ptr()) })
}

/// Bridges two sessions by UUID (originator ↔ originatee).
pub fn uuid_bridge(
    originator_uuid: impl AsRef<str>,
    originatee_uuid: impl AsRef<str>,
) -> Result<()> {
    let a = cstring(originator_uuid)?;
    let b = cstring(originatee_uuid)?;
    // SAFETY: two valid uuid C strings.
    status_to_result(unsafe { sys::switch_ivr_uuid_bridge(a.as_ptr(), b.as_ptr()) })
}

/// Hangs up the session identified by `uuid` with `cause`.
pub fn kill_uuid(uuid: impl AsRef<str>, cause: Cause) -> Result<()> {
    let uuid = cstring(uuid)?;
    // SAFETY: valid uuid; `cause.raw()` is a valid switch_call_cause_t.
    status_to_result(unsafe { sys::switch_ivr_kill_uuid(uuid.as_ptr(), cause.raw()) })
}

// ── originate / eavesdrop / intercept / schedule / detect ──────────────────

/// Schedules a broadcast of `path` onto `uuid` at `runtime` (epoch seconds). Returns the task id.
pub fn schedule_broadcast(
    runtime: i64,
    uuid: impl AsRef<str>,
    path: impl AsRef<str>,
    flags: sys::switch_media_flag_t,
) -> u32 {
    let uuid = match cstring(uuid) {
        Ok(s) => s,
        Err(_) => return 0,
    };
    let path = match cstring(path) {
        Ok(s) => s,
        Err(_) => return 0,
    };
    // SAFETY: plain int; two valid C strings; valid flags.
    unsafe {
        sys::switch_ivr_schedule_broadcast(
            runtime as sys::time_t,
            uuid.as_ptr(),
            path.as_ptr(),
            flags,
        )
    }
}

/// Schedules a hangup of `uuid` at `runtime` with `cause` (optionally the b-leg). Returns task id.
pub fn schedule_hangup(runtime: i64, uuid: impl AsRef<str>, cause: Cause, bleg: bool) -> u32 {
    let uuid = match cstring(uuid) {
        Ok(s) => s,
        Err(_) => return 0,
    };
    let bleg = if bleg {
        sys::switch_bool_t_SWITCH_TRUE
    } else {
        sys::switch_bool_t_SWITCH_FALSE
    };
    // SAFETY: plain int; valid uuid; valid cause; valid bool.
    unsafe {
        sys::switch_ivr_schedule_hangup(runtime as sys::time_t, uuid.as_ptr(), cause.raw(), bleg)
    }
}

/// Schedules a transfer of `uuid` at `runtime`. `extension`/`dialplan`/`context` are owned C
/// strings freed by FS (pass via `cstring`). Returns task id.
///
/// # Safety
/// The three `*_ptr` args must be heap-allocated, NUL-terminated, and owned by FreeSWITCH after
/// the call (it frees them). Use [`cstring`] to produce them.
pub unsafe fn schedule_transfer(
    runtime: i64,
    uuid: impl AsRef<str>,
    extension_ptr: *mut std::os::raw::c_char,
    dialplan_ptr: *mut std::os::raw::c_char,
    context_ptr: *mut std::os::raw::c_char,
) -> u32 {
    let uuid = match cstring(uuid) {
        Ok(s) => s,
        Err(_) => return 0,
    };
    // SAFETY: caller guarantees the three ptrs are heap-allocated + FS-owned; valid uuid.
    unsafe {
        sys::switch_ivr_schedule_transfer(
            runtime as sys::time_t,
            uuid.as_ptr(),
            extension_ptr,
            dialplan_ptr,
            context_ptr,
        )
    }
}

// ── TTS speech interface (switch_core_speech_*) ────────────────────────────
// These wrap the low-level TTS engine interface (`switch_speech_handle_t`). The handle is an
// opaque struct the caller allocates (e.g. on a pool). All `unsafe` stays inside.

/// Opens a TTS speech handle: `module_name` is the TTS engine (e.g. `"flite"`, `"cepstral"`),
/// `voice_name` the voice, `rate`/`interval`/`channels` the audio format, `flags` an in/out
/// `switch_speech_flag_t`, `pool` the APR pool for the handle's allocations.
pub fn speech_open(
    sh: *mut sys::switch_speech_handle_t,
    module_name: impl AsRef<str>,
    voice_name: impl AsRef<str>,
    rate: u32,
    interval: u32,
    channels: u32,
    flags: *mut sys::switch_speech_flag_t,
    pool: &Pool,
) -> Result<()> {
    let module = cstring(module_name)?;
    let voice = cstring(voice_name)?;
    // SAFETY: `sh` valid per caller; two valid C strings; plain ints; `flags` in/out; live pool.
    status_to_result(unsafe {
        sys::switch_core_speech_open(
            sh,
            module.as_ptr(),
            voice.as_ptr(),
            rate as _,
            interval as _,
            channels as _,
            flags,
            pool.as_ptr(),
        )
    })
}

/// Feeds `text` to the TTS engine for synthesis. `flags` is an in/out `switch_speech_flag_t`.
pub fn speech_feed_tts(
    sh: *mut sys::switch_speech_handle_t,
    text: impl AsRef<str>,
    flags: *mut sys::switch_speech_flag_t,
) -> Result<()> {
    let text = cstring(text)?;
    // SAFETY: `sh` live; valid C string; `flags` in/out.
    status_to_result(unsafe { sys::switch_core_speech_feed_tts(sh, text.as_ptr(), flags) })
}

/// Reads synthesized audio from the TTS engine into `data`. `datalen` is in/out (max on input,
/// actual on output). `flags` in/out.
pub fn speech_read_tts(
    sh: *mut sys::switch_speech_handle_t,
    data: *mut std::ffi::c_void,
    datalen: &mut u64,
    flags: *mut sys::switch_speech_flag_t,
) -> Result<()> {
    let mut n: sys::switch_size_t = *datalen as _;
    // SAFETY: `sh` live; `data` valid buffer for `*datalen`; `&mut n` valid in/out; `flags` in/out.
    let s = unsafe { sys::switch_core_speech_read_tts(sh, data, &mut n, flags) };
    *datalen = n as u64;
    status_to_result(s)
}

/// Flushes the TTS engine's buffer (discards pending synthesis).
pub fn speech_flush_tts(sh: *mut sys::switch_speech_handle_t) {
    // SAFETY: `sh` live.
    unsafe { sys::switch_core_speech_flush_tts(sh) };
}

/// Sets a text parameter on the TTS engine (e.g. `"voice"` → `"alice"`).
pub fn speech_text_param_tts(
    sh: *mut sys::switch_speech_handle_t,
    param: impl AsRef<str>,
    val: impl AsRef<str>,
) -> Result<()> {
    let param = cstring(param)?;
    let val = cstring(val)?;
    // SAFETY: `sh` live; two valid C strings (FS copies into pool; the *mut on `param` is a
    // non-const convention, not a mutation of our CString).
    unsafe {
        sys::switch_core_speech_text_param_tts(sh, param.as_ptr() as *mut _, val.as_ptr());
    }
    Ok(())
}

/// Sets a numeric parameter on the TTS engine.
pub fn speech_numeric_param_tts(
    sh: *mut sys::switch_speech_handle_t,
    param: impl AsRef<str>,
    val: i32,
) -> Result<()> {
    let param = cstring(param)?;
    // SAFETY: `sh` live; valid C string; plain int.
    unsafe {
        sys::switch_core_speech_numeric_param_tts(sh, param.as_ptr() as *mut _, val);
    }
    Ok(())
}

/// Sets a float parameter on the TTS engine.
pub fn speech_float_param_tts(
    sh: *mut sys::switch_speech_handle_t,
    param: impl AsRef<str>,
    val: f64,
) -> Result<()> {
    let param = cstring(param)?;
    // SAFETY: `sh` live; valid C string; plain f64.
    unsafe {
        sys::switch_core_speech_float_param_tts(sh, param.as_ptr() as *mut _, val);
    }
    Ok(())
}

/// Closes the TTS speech handle (releases engine resources). `flags` in/out.
pub fn speech_close(
    sh: *mut sys::switch_speech_handle_t,
    flags: *mut sys::switch_speech_flag_t,
) -> Result<()> {
    // SAFETY: `sh` live; `flags` in/out.
    status_to_result(unsafe { sys::switch_core_speech_close(sh, flags) })
}

// ---------------------------------------------------------------------------
// Session IVR methods (switch_ivr_*)
// ---------------------------------------------------------------------------
// Migrated from module-level free functions that took `session: Session` as their first
// parameter. `Session` is a `Copy` handle, so these methods take `self` by value. The two-session
// bridges (`multi_threaded_bridge*`, `signal_bridge`) take the originator as `self` and the peer
// as a plain parameter. `session_transfer` was renamed to `transfer`.
impl Session {
    /// Records the session's media to `path`. `limit` is the maximum recording length in seconds
    /// (pass `0` for no limit). A null file handle lets FreeSWITCH open `path` itself.
    pub fn record_file(self, path: impl AsRef<str>, limit: u32) -> Result<()> {
        let path = cstring(path)?;
        // SAFETY: `self.as_ptr()` is a live session; `path` is a valid C string; null file handle
        // and input args select the default recording behavior.
        let status = unsafe {
            sys::switch_ivr_record_file(
                self.as_ptr(),
                std::ptr::null_mut(),
                path.as_ptr(),
                std::ptr::null_mut(),
                limit,
            )
        };
        status_to_result(status)
    }

    /// Parks the session. A `SWITCH_STATUS_FALSE`/break result indicates the channel left the park
    /// (typically a hangup); it is surfaced as `Err` like any non-success status.
    pub fn park(self) -> Result<()> {
        // SAFETY: `self.as_ptr()` is a live session; a null input args pointer is permitted.
        let status = unsafe { sys::switch_ivr_park(self.as_ptr(), std::ptr::null_mut()) };
        status_to_result(status)
    }

    /// Collects digits from the session using a registered input callback. The `digit_timeout` and
    /// `abs_timeout` values are in milliseconds; pass `0` for no timeout on either.
    ///
    /// The input-args struct (`switch_input_args_t`) is currently passed as null, which selects the
    /// default (no custom callback) behavior. A non-success status is surfaced as `Err`.
    pub fn collect_digits_callback(self, digit_timeout: u32, abs_timeout: u32) -> Result<()> {
        // SAFETY: `self.as_ptr()` is a live session; a null input-args pointer is permitted.
        let status = unsafe {
            sys::switch_ivr_collect_digits_callback(
                self.as_ptr(),
                std::ptr::null_mut(),
                digit_timeout,
                abs_timeout,
            )
        };
        status_to_result(status)
    }

    /// Collects up to `max_digits` digits into `buf`, stopping early when a character from
    /// `terminators` is pressed. On success the number of digits collected is written to `out_count`.
    ///
    /// `first_timeout` is the wait for the first digit, `digit_timeout` the inter-digit wait, and
    /// `abs_timeout` an absolute cap — all in milliseconds (`0` means no timeout). Any pressed
    /// terminator character is written to `out_terminator` (an empty string when none matched).
    #[allow(clippy::too_many_arguments)]
    pub fn collect_digits_count(
        self,
        buf: &mut [u8],
        max_digits: usize,
        terminators: impl AsRef<str>,
        first_timeout: u32,
        digit_timeout: u32,
        abs_timeout: u32,
        out_count: &mut usize,
        out_terminator: &mut String,
    ) -> Result<()> {
        let terminators = cstring(terminators)?;
        let mut term_byte: MaybeUninit<c_char> = MaybeUninit::uninit();
        // SAFETY: `self.as_ptr()` is a live session; `buf` is a writable byte buffer of length
        // `buflen` for the duration of the call; `terminators` is a valid C string; `term_byte` is a
        // valid single-byte output location.
        let status = unsafe {
            sys::switch_ivr_collect_digits_count(
                self.as_ptr(),
                buf.as_mut_ptr().cast::<c_char>(),
                buf.len(),
                max_digits,
                terminators.as_ptr(),
                term_byte.as_mut_ptr(),
                first_timeout,
                digit_timeout,
                abs_timeout,
            )
        };
        // SAFETY: `term_byte` is initialized by the callee on return (to NUL when no terminator fired).
        let term = unsafe { term_byte.assume_init() };
        if term != 0 {
            out_terminator.clear();
            out_terminator.push(term as u8 as char);
        } else {
            out_terminator.clear();
        }
        // The number of digits written is not returned by the C API; approximate it as the position of
        // the first NUL byte in `buf`.
        *out_count = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        status_to_result(status)
    }

    /// The main digit-collection primitive: prompts with `prompt_audio_file` (may be empty), then reads
    /// between `min_digits` and `max_digits` digits into `buf`, storing them in channel variable
    /// `var_name` when given. Stops early on any character from `valid_terminators`.
    ///
    /// `timeout` is the wait for the first digit and `digit_timeout` the inter-digit wait, in
    /// milliseconds (`0` means no timeout). Pass an empty string for `prompt_audio_file` or
    /// `var_name` to omit them. On success the number of digits collected is written to `out_count`.
    #[allow(clippy::too_many_arguments)]
    pub fn read(
        self,
        min_digits: u32,
        max_digits: u32,
        prompt_audio_file: impl AsRef<str>,
        var_name: impl AsRef<str>,
        buf: &mut [u8],
        timeout: u32,
        valid_terminators: impl AsRef<str>,
        digit_timeout: u32,
        out_count: &mut usize,
    ) -> Result<()> {
        let prompt = cstring(prompt_audio_file)?;
        let var = cstring(var_name)?;
        let terminators = cstring(valid_terminators)?;
        // SAFETY: `self.as_ptr()` is a live session; `prompt`, `var`, and `terminators` are valid C
        // strings for the call; `buf` is a writable byte buffer of length `digit_buffer_length`.
        let status = unsafe {
            sys::switch_ivr_read(
                self.as_ptr(),
                min_digits,
                max_digits,
                prompt.as_ptr(),
                var.as_ptr(),
                buf.as_mut_ptr().cast::<c_char>(),
                buf.len(),
                timeout,
                terminators.as_ptr(),
                digit_timeout,
            )
        };
        *out_count = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        status_to_result(status)
    }

    /// Records the entire session to `file`. `limit` is the maximum length in seconds (`0` for no
    /// limit). The file-handle pointer is currently null, letting FreeSWITCH manage the file itself.
    pub fn record_session(self, file: impl AsRef<str>, limit: u32) -> Result<()> {
        let file = cstring(file)?;
        // SAFETY: `self.as_ptr()` is a live session; `file` is a valid C string; a null file handle
        // selects the default file management.
        let status = unsafe {
            sys::switch_ivr_record_session(
                self.as_ptr(),
                file.as_ptr(),
                limit,
                std::ptr::null_mut(),
            )
        };
        status_to_result(status)
    }

    /// Like [`record_session`](Self::record_session) but seeds the recording with the given channel
    /// `variables` event. The file-handle pointer is currently null.
    pub fn record_session_event(
        self,
        file: impl AsRef<str>,
        limit: u32,
        variables: &crate::Event,
    ) -> Result<()> {
        let file = cstring(file)?;
        // SAFETY: `self.as_ptr()` is a live session; `file` is a valid C string; a null file handle
        // selects the default file management; `variables.as_ptr()` is a live event pointer borrowed
        // for the duration of the call.
        let status = unsafe {
            sys::switch_ivr_record_session_event(
                self.as_ptr(),
                file.as_ptr(),
                limit,
                std::ptr::null_mut(),
                variables.as_ptr(),
            )
        };
        status_to_result(status)
    }

    /// Masks (or unmasks) recording of `file` on the session. Pass `on = true` to mask, `false` to
    /// unmask. Masking suppresses captured audio for the file's recording without stopping it.
    pub fn record_session_mask(self, file: impl AsRef<str>, on: bool) -> Result<()> {
        let file = cstring(file)?;
        let on = if on {
            sys::switch_bool_t_SWITCH_TRUE
        } else {
            sys::switch_bool_t_SWITCH_FALSE
        };
        // SAFETY: `self.as_ptr()` is a live session; `file` is a valid C string.
        let status =
            unsafe { sys::switch_ivr_record_session_mask(self.as_ptr(), file.as_ptr(), on) };
        status_to_result(status)
    }

    /// Pauses (or resumes) recording of `file` on the session. Pass `on = true` to pause, `false` to
    /// resume.
    pub fn record_session_pause(self, file: impl AsRef<str>, on: bool) -> Result<()> {
        let file = cstring(file)?;
        let on = if on {
            sys::switch_bool_t_SWITCH_TRUE
        } else {
            sys::switch_bool_t_SWITCH_FALSE
        };
        // SAFETY: `self.as_ptr()` is a live session; `file` is a valid C string.
        let status =
            unsafe { sys::switch_ivr_record_session_pause(self.as_ptr(), file.as_ptr(), on) };
        status_to_result(status)
    }

    /// Displaces the session's audio with the contents of `file`. `limit` is the maximum length in
    /// seconds (`0` for no limit). `flags` is a FreeSWITCH displace flag string (e.g. `"w"` to write,
    /// `"r"` to read, `"l"` to loop).
    pub fn displace_session(
        self,
        file: impl AsRef<str>,
        limit: u32,
        flags: impl AsRef<str>,
    ) -> Result<()> {
        let file = cstring(file)?;
        let flags = cstring(flags)?;
        // SAFETY: `self.as_ptr()` is a live session; `file` and `flags` are valid C strings.
        let status = unsafe {
            sys::switch_ivr_displace_session(self.as_ptr(), file.as_ptr(), limit, flags.as_ptr())
        };
        status_to_result(status)
    }

    /// Plays a delayed echo of the session's audio back to the caller. `delay_ms` is the echo delay in
    /// milliseconds. The C function returns no status; errors (if any) are surfaced via the underlying
    /// media layer rather than here.
    pub fn delay_echo(self, delay_ms: u32) {
        // SAFETY: `self.as_ptr()` is a live session; `delay_ms` is a plain value.
        unsafe { sys::switch_ivr_delay_echo(self.as_ptr(), delay_ms) };
    }

    /// Blocks DTMF on the session. Subsequent DTMF events are queued rather than delivered to the
    /// channel until [`unblock_dtmf_session`](Self::unblock_dtmf_session) is called.
    pub fn block_dtmf_session(self) -> Result<()> {
        // SAFETY: `self.as_ptr()` is a live session.
        let status = unsafe { sys::switch_ivr_block_dtmf_session(self.as_ptr()) };
        status_to_result(status)
    }

    /// Unblocks previously-blocked DTMF on the session, releasing queued events.
    pub fn unblock_dtmf_session(self) -> Result<()> {
        // SAFETY: `self.as_ptr()` is a live session.
        let status = unsafe { sys::switch_ivr_unblock_dtmf_session(self.as_ptr()) };
        status_to_result(status)
    }

    /// Enables or disables RFC 4103 real-time text capture on the session.
    pub fn capture_text(self, on: bool) -> Result<()> {
        let on = if on {
            sys::switch_bool_t_SWITCH_TRUE
        } else {
            sys::switch_bool_t_SWITCH_FALSE
        };
        // SAFETY: `self.as_ptr()` is a live session.
        let status = unsafe { sys::switch_ivr_capture_text(self.as_ptr(), on) };
        status_to_result(status)
    }

    /// Returns `true` when the session's channel is currently on hold.
    ///
    /// The underlying `switch_ivr_check_hold` returns no status, so this never fails; the answer is
    /// derived from the channel's `CF_HOLD` flag, which is the same flag the C function consults.
    pub fn check_hold(self) -> bool {
        // SAFETY: `self.as_ptr()` is a live session; the function is a pure query with no outputs.
        unsafe { sys::switch_ivr_check_hold(self.as_ptr()) };
        self.channel()
            .is_some_and(|ch| ch.test_flag(crate::ChannelFlag::HOLD))
    }

    /// Broadcasts `app` to the session from a new background thread. `flags` is an opaque integer
    /// passed through to the application. Returns an error if `app` contains an interior NUL.
    ///
    /// `switch_ivr_broadcast_in_thread` stores the `app` pointer and spawns a detached thread that
    /// reads it asynchronously (verified against `switch_ivr_async.c`: `bch->app = app` without
    /// `strdup`). The pointer must therefore outlive the caller's stack frame. This wrapper copies
    /// `app` into the session's memory pool via `switch_core_session_strdup` so the spawned thread's
    /// reference remains valid for the session's lifetime.
    pub fn broadcast_in_thread(self, app: impl AsRef<str>, flags: i32) -> Result<()> {
        let app = cstring(app)?;
        // SAFETY: `self.as_ptr()` is a live session; `app` is a valid C string for this call. The
        // returned pointer is allocated from the session's pool and lives for the session's lifetime,
        // so the spawned thread's `bch->app` reference is valid.
        let pooled = unsafe {
            sys::switch_core_perform_session_strdup(
                self.as_ptr(),
                app.as_ptr(),
                c"fswtch-rs".as_ptr(),
                c"broadcast_in_thread".as_ptr(),
                line!() as _,
            )
        };
        if pooled.is_null() {
            return Err(crate::SwitchError(crate::GENERR));
        }
        // SAFETY: `self.as_ptr()` is a live session; `pooled` is a pool-allocated C string valid for
        // the session's lifetime, so the spawned thread's async read is sound.
        unsafe { sys::switch_ivr_broadcast_in_thread(self.as_ptr(), pooled, flags) };
        Ok(())
    }

    /// Runs the dialplan application `app` (with argument `arg`) against every session currently
    /// eavesdropping on `session`.
    pub fn eavesdrop_exec_all(self, app: impl AsRef<str>, arg: &str) -> Result<()> {
        let app = cstring(app)?;
        let arg = cstring(arg)?;
        // SAFETY: `self.as_ptr()` is a live session; `app` and `arg` are valid C strings.
        let status = unsafe {
            sys::switch_ivr_eavesdrop_exec_all(self.as_ptr(), app.as_ptr(), arg.as_ptr())
        };
        status_to_result(status)
    }

    /// Pops the next session eavesdropping on `session` and returns it. Returns `Ok(None)` when no
    /// eavesdropper is present. The returned [`Session`] is a non-owning wrapper; the caller must not
    /// use it after the originating session is destroyed.
    pub fn eavesdrop_pop_eavesdropper(self) -> Result<Option<Session>> {
        let mut out: *mut sys::switch_core_session_t = std::ptr::null_mut();
        // SAFETY: `self.as_ptr()` is a live session; `out` is a valid output location for a session
        // pointer.
        let status = unsafe {
            sys::switch_ivr_eavesdrop_pop_eavesdropper(self.as_ptr(), &mut out as *mut _)
        };
        status_to_result(status)?;
        // SAFETY: `out` is either null or a live session pointer provided by FreeSWITCH.
        Ok(unsafe { Session::from_raw(out) })
    }

    /// Originates a call from `session` to `bridge_to` (a dial string such as `"user/1000"` or
    /// `"sofia/gateway/mygw/15551234"`).
    ///
    /// `timelimit_sec` caps the ringing/answer window. `cid_name` / `cid_num`, when given, override the
    /// caller-id presented on the B-leg. The complex parameters (state-handler table, caller-profile
    /// override, variable event, dial handle) are passed as null — use
    /// [`originate_raw`](Self::originate_raw) to supply them.
    ///
    /// On success the returned `peer` is the answered B-leg session and `cause` is
    /// `CAUSE_NORMAL_CLEARING`; on failure `peer` is `None` and `cause` carries the disconnect reason.
    /// The originate *status* is mapped to `Result`: a non-success status returns `Err`.
    pub fn originate(
        self,
        bridge_to: impl AsRef<str>,
        timelimit_sec: u32,
        cid_name: Option<&str>,
        cid_num: Option<&str>,
    ) -> Result<OriginateOutcome> {
        let bridge_to = cstring(bridge_to)?;
        let cid_name = cid_name.map(cstring).transpose()?;
        let cid_num = cid_num.map(cstring).transpose()?;
        self.originate_raw(
            bridge_to.as_ptr(),
            timelimit_sec,
            std::ptr::null(),
            cid_name.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
            cid_num.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
        )
    }

    /// Full-featured originate escape hatch, exposing every parameter of `switch_ivr_originate`.
    ///
    /// # Safety / contract
    ///
    /// - `bridge_to` must be a valid null-terminated C string for the call.
    /// - `cid_name` / `cid_num`, when non-null, must be valid C strings for the call.
    /// - `caller_profile_override` must be null or a valid `switch_caller_profile_t *` whose lifetime
    ///   outlasts the call.
    /// - `ovars` must be null or a valid `switch_event_t *` whose lifetime outlasts the call.
    /// - `table` must be null or a valid `switch_state_handler_table_t *` whose lifetime outlasts the
    ///   call.
    /// - `dh` must be null or a valid `switch_dial_handle_t *` (built by FreeSWITCH's dial-handle API)
    ///   whose lifetime outlasts the call.
    /// - `flags` is a bitmask of `SOF_*` originate flags (see `sys::switch_originate_flag_enum_t_*`).
    ///
    /// See [`originate`](Self::originate) for the meaning of the returned `OriginateOutcome`.
    #[allow(clippy::too_many_arguments)]
    pub fn originate_raw(
        self,
        bridge_to: *const c_char,
        timelimit_sec: u32,
        table: *const sys::switch_state_handler_table_t,
        cid_name: *const c_char,
        cid_num: *const c_char,
        caller_profile_override: *mut sys::switch_caller_profile_t,
        ovars: *mut sys::switch_event_t,
        flags: sys::switch_originate_flag_t,
        dh: *mut sys::switch_dial_handle_t,
    ) -> Result<OriginateOutcome> {
        let mut peer: *mut sys::switch_core_session_t = std::ptr::null_mut();
        let mut cause: sys::switch_call_cause_t = 0;
        let mut cancel_cause: sys::switch_call_cause_t = 0;
        // SAFETY: `self.as_ptr()` is a live session; `bridge_to`/`cid_*` are valid C strings or null
        // per the caller's contract; the out-pointers are valid stack slots; the complex raw pointers
        // are valid or null per the caller's contract.
        let status = unsafe {
            sys::switch_ivr_originate(
                self.as_ptr(),
                &mut peer,
                &mut cause,
                bridge_to,
                timelimit_sec,
                table,
                cid_name,
                cid_num,
                caller_profile_override,
                ovars,
                flags,
                &mut cancel_cause,
                dh,
            )
        };
        // SAFETY: `peer` was just produced by FreeSWITCH as a live session when non-null.
        let peer = unsafe { Session::from_raw(peer) };
        status_to_result(status)?;
        Ok(OriginateOutcome {
            peer,
            cause: Cause::from_raw(cause),
            cancel_cause: Cause::from_raw(cancel_cause),
        })
    }

    /// Bridges two sessions on the current thread: `self` (A-leg) and `peer` (B-leg). The bridge runs
    /// until one side hangs up or breaks. A null dtmf callback and null per-session user data select the
    /// default bridging behavior; use [`multi_threaded_bridge_raw`](Self::multi_threaded_bridge_raw)
    /// to supply them.
    pub fn multi_threaded_bridge(self, peer: Session) -> Result<()> {
        self.multi_threaded_bridge_raw(peer, None, std::ptr::null_mut(), std::ptr::null_mut())
    }

    /// Full-featured [`multi_threaded_bridge`](Self::multi_threaded_bridge) escape hatch, accepting
    /// an optional input callback and opaque per-session user data pointers. The callback and user
    /// data must remain valid for the duration of the bridge.
    pub fn multi_threaded_bridge_raw(
        self,
        peer: Session,
        dtmf_callback: sys::switch_input_callback_function_t,
        session_data: *mut std::ffi::c_void,
        peer_session_data: *mut std::ffi::c_void,
    ) -> Result<()> {
        // SAFETY: both session pointers are live; the callback (when provided) is a valid function
        // pointer; the user-data pointers are opaque to FreeSWITCH and valid per the caller's contract.
        let status = unsafe {
            sys::switch_ivr_multi_threaded_bridge(
                self.as_ptr(),
                peer.as_ptr(),
                dtmf_callback,
                session_data,
                peer_session_data,
            )
        };
        status_to_result(status)
    }

    /// Pronounces `to_say` on `session` using the named say module, type, method, and gender.
    ///
    /// `module_name` selects the say engine (e.g. `"en"`); `say_type` is one of `"NUMBER"`, `"ITEMS"`,
    /// `"PERSONS"`, `"CURRENCY"`, `"CURRENT_DATE_TIME"`, `"TELEPHONE_NUMBER"`, etc.; `say_method` is
    /// one of `"PRONOUNCED"`, `"ITERATED"`, `"COUNTED"`, `"PRONOUNCED_YEAR"`; `say_gender` may be
    /// `"NEUTER"` or `None`. A null input-args pointer selects the default behavior.
    pub fn say(
        self,
        to_say: impl AsRef<str>,
        module_name: impl AsRef<str>,
        say_type: impl AsRef<str>,
        say_method: impl AsRef<str>,
        say_gender: Option<&str>,
    ) -> Result<()> {
        let to_say = cstring(to_say)?;
        let module_name = cstring(module_name)?;
        let say_type = cstring(say_type)?;
        let say_method = cstring(say_method)?;
        let say_gender = say_gender.map(cstring).transpose()?;
        // SAFETY: `self.as_ptr()` is live; all string pointers are valid C strings for the call; a
        // null input-args pointer is permitted.
        let status = unsafe {
            sys::switch_ivr_say(
                self.as_ptr(),
                to_say.as_ptr(),
                module_name.as_ptr(),
                say_type.as_ptr(),
                say_method.as_ptr(),
                say_gender.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
                std::ptr::null_mut(),
            )
        };
        status_to_result(status)
    }

    /// Computes the pronunciation string for `to_say` without speaking it, using the given `lang` and
    /// `ext` (module extension), say type/method/gender. Returns the produced string (an owned copy of
    /// FreeSWITCH-allocated storage, freed before returning).
    #[allow(clippy::too_many_arguments)]
    pub fn say_string(
        self,
        lang: Option<&str>,
        ext: Option<&str>,
        to_say: impl AsRef<str>,
        module_name: impl AsRef<str>,
        say_type: impl AsRef<str>,
        say_method: impl AsRef<str>,
        say_gender: Option<&str>,
    ) -> Result<Option<String>> {
        let lang = lang.map(cstring).transpose()?;
        let ext = ext.map(cstring).transpose()?;
        let to_say = cstring(to_say)?;
        let module_name = cstring(module_name)?;
        let say_type = cstring(say_type)?;
        let say_method = cstring(say_method)?;
        let say_gender = say_gender.map(cstring).transpose()?;
        let mut rstr: *mut c_char = std::ptr::null_mut();
        // SAFETY: `self.as_ptr()` is live; all string pointers are valid C strings or null for the
        // call; `rstr` is a valid out-pointer. On SUCCESS `rstr` is malloc'd by FreeSWITCH.
        let status = unsafe {
            sys::switch_ivr_say_string(
                self.as_ptr(),
                lang.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
                ext.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
                to_say.as_ptr(),
                module_name.as_ptr(),
                say_type.as_ptr(),
                say_method.as_ptr(),
                say_gender.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
                &mut rstr,
            )
        };
        status_to_result(status)?;
        // SAFETY: on SUCCESS `rstr` is null or a malloc'd C string that has not been freed.
        Ok(unsafe { strdup_to_string(rstr) })
    }

    /// Spells `to_say` on `session` using the given `switch_say_args_t` (built by the caller). A null
    /// input-args pointer selects the default behavior.
    pub fn say_spell(
        self,
        to_say: impl AsRef<str>,
        say_args: *mut sys::switch_say_args_t,
    ) -> Result<()> {
        let to_say = cstring(to_say)?;
        // SAFETY: `self.as_ptr()` is live; `to_say` is a valid C string for the call; `say_args` is
        // valid or null per the caller's contract; a null input-args pointer is permitted.
        let status = unsafe {
            sys::switch_ivr_say_spell(
                self.as_ptr(),
                to_say.as_ptr() as *mut c_char,
                say_args,
                std::ptr::null_mut(),
            )
        };
        status_to_result(status)
    }

    /// Pronounces an IP address on `session`, using the provided `number_func` (a say callback) to
    /// vocalize each octet, plus the given `switch_say_args_t`. A null input-args pointer selects the
    /// default behavior.
    pub fn say_ip(
        self,
        to_say: impl AsRef<str>,
        number_func: sys::switch_say_callback_t,
        say_args: *mut sys::switch_say_args_t,
    ) -> Result<()> {
        let to_say = cstring(to_say)?;
        // SAFETY: `self.as_ptr()` is live; `to_say` is a valid C string for the call; `number_func`
        // and `say_args` are valid or null per the caller's contract; a null input-args pointer is
        // permitted.
        let status = unsafe {
            sys::switch_ivr_say_ip(
                self.as_ptr(),
                to_say.as_ptr() as *mut c_char,
                number_func,
                say_args,
                std::ptr::null_mut(),
            )
        };
        status_to_result(status)
    }

    /// Parses a single queued event for `session` (DTMF, custom events, etc.). Pass the event via its
    /// `*mut switch_event_t` pointer (the [`crate::EventRef`] / [`crate::Event`] escape hatch). A null
    /// event pointer selects the default behavior.
    pub fn parse_event(self, event: *mut sys::switch_event_t) -> Result<()> {
        // SAFETY: `self.as_ptr()` is live; `event` is null or a valid event pointer per the caller's
        // contract.
        let status = unsafe { sys::switch_ivr_parse_event(self.as_ptr(), event) };
        status_to_result(status)
    }

    /// Parses all queued events for `session`.
    pub fn parse_all_events(self) -> Result<()> {
        // SAFETY: `self.as_ptr()` is a live session.
        let status = unsafe { sys::switch_ivr_parse_all_events(self.as_ptr()) };
        status_to_result(status)
    }

    /// Parses the next single queued event for `session`.
    pub fn parse_next_event(self) -> Result<()> {
        // SAFETY: `self.as_ptr()` is a live session.
        let status = unsafe { sys::switch_ivr_parse_next_event(self.as_ptr()) };
        status_to_result(status)
    }

    /// Generates a JSON CDR document for `session`. `urlencode` controls whether the body is URL-encoded.
    /// The returned `*mut cJSON` is owned by the caller and must be freed with `cJSON_Delete` (the
    /// underlying `cJSON` type is opaque to this wrapper).
    pub fn generate_json_cdr(self, urlencode: bool) -> Result<*mut sys::cJSON> {
        let mut json: *mut sys::cJSON = std::ptr::null_mut();
        // SAFETY: `self.as_ptr()` is live; `json` is a valid out-pointer.
        let status = unsafe {
            sys::switch_ivr_generate_json_cdr(
                self.as_ptr(),
                &mut json,
                if urlencode {
                    sys::switch_bool_t_SWITCH_TRUE
                } else {
                    sys::switch_bool_t_SWITCH_FALSE
                },
            )
        };
        status_to_result(status)?;
        Ok(json)
    }

    /// Generates an XML CDR document for `session`. The returned `switch_xml_t` is owned by the caller
    /// and must be freed with `switch_xml_free` (the underlying `switch_xml` type is opaque to this
    /// wrapper); a null out-slot selects a freshly allocated document.
    pub fn generate_xml_cdr(self) -> Result<sys::switch_xml_t> {
        let mut xml: sys::switch_xml_t = std::ptr::null_mut();
        // SAFETY: `self.as_ptr()` is live; `xml` is a valid out-pointer (null slot => fresh doc).
        let status = unsafe { sys::switch_ivr_generate_xml_cdr(self.as_ptr(), &mut xml) };
        status_to_result(status)?;
        Ok(xml)
    }

    /// Initializes ASR speech detection on `session` using the named module (`mod_name`, e.g.
    /// `"pocketsphinx"`) and `dest` (the recognition destination/path). The `switch_asr_handle_t` slot
    /// (`ah`) is filled by FreeSWITCH; pass a pointer to a `switch_asr_handle_t`.
    pub fn detect_speech_init(
        self,
        mod_name: impl AsRef<str>,
        dest: impl AsRef<str>,
        ah: *mut sys::switch_asr_handle_t,
    ) -> Result<()> {
        let mod_name = cstring(mod_name)?;
        let dest = cstring(dest)?;
        // SAFETY: `self.as_ptr()` is live; both strings are valid C strings for the call; `ah` is a
        // valid pointer to a `switch_asr_handle_t` per the caller's contract.
        let status = unsafe {
            sys::switch_ivr_detect_speech_init(self.as_ptr(), mod_name.as_ptr(), dest.as_ptr(), ah)
        };
        status_to_result(status)
    }

    /// Starts ASR speech detection on `session` using `mod_name`, loading `grammar` under the name
    /// `name`, with recognition `dest`. The `switch_asr_handle_t` slot (`ah`) is filled by FreeSWITCH.
    pub fn detect_speech(
        self,
        mod_name: impl AsRef<str>,
        grammar: impl AsRef<str>,
        name: impl AsRef<str>,
        dest: impl AsRef<str>,
        ah: *mut sys::switch_asr_handle_t,
    ) -> Result<()> {
        let mod_name = cstring(mod_name)?;
        let grammar = cstring(grammar)?;
        let name = cstring(name)?;
        let dest = cstring(dest)?;
        // SAFETY: `self.as_ptr()` is live; all strings are valid C strings for the call; `ah` is a
        // valid pointer per the caller's contract.
        let status = unsafe {
            sys::switch_ivr_detect_speech(
                self.as_ptr(),
                mod_name.as_ptr(),
                grammar.as_ptr(),
                name.as_ptr(),
                dest.as_ptr(),
                ah,
            )
        };
        status_to_result(status)
    }

    /// Stops ASR speech detection on `session`.
    pub fn stop_detect_speech(self) -> Result<()> {
        // SAFETY: `self.as_ptr()` is a live session.
        let status = unsafe { sys::switch_ivr_stop_detect_speech(self.as_ptr()) };
        status_to_result(status)
    }

    /// Pauses ASR speech detection on `session`.
    pub fn pause_detect_speech(self) -> Result<()> {
        // SAFETY: `self.as_ptr()` is a live session.
        let status = unsafe { sys::switch_ivr_pause_detect_speech(self.as_ptr()) };
        status_to_result(status)
    }

    /// Resumes ASR speech detection on `session`.
    pub fn resume_detect_speech(self) -> Result<()> {
        // SAFETY: `self.as_ptr()` is a live session.
        let status = unsafe { sys::switch_ivr_resume_detect_speech(self.as_ptr()) };
        status_to_result(status)
    }

    /// Loads a named ASR `grammar` on `session`, registering it under `name`.
    pub fn detect_speech_load_grammar(
        self,
        grammar: impl AsRef<str>,
        name: impl AsRef<str>,
    ) -> Result<()> {
        let grammar = cstring(grammar)?;
        let name = cstring(name)?;
        // SAFETY: `self.as_ptr()` is live; both strings are valid C strings for the call.
        let status = unsafe {
            sys::switch_ivr_detect_speech_load_grammar(
                self.as_ptr(),
                grammar.as_ptr(),
                name.as_ptr(),
            )
        };
        status_to_result(status)
    }

    /// Unloads the named ASR grammar (`name`) from `session`.
    pub fn detect_speech_unload_grammar(self, name: impl AsRef<str>) -> Result<()> {
        let name = cstring(name)?;
        // SAFETY: `self.as_ptr()` is live; `name` is a valid C string for the call.
        let status =
            unsafe { sys::switch_ivr_detect_speech_unload_grammar(self.as_ptr(), name.as_ptr()) };
        status_to_result(status)
    }

    /// Enables the named ASR grammar (`name`) on `session`.
    pub fn detect_speech_enable_grammar(self, name: impl AsRef<str>) -> Result<()> {
        let name = cstring(name)?;
        // SAFETY: `self.as_ptr()` is live; `name` is a valid C string for the call.
        let status =
            unsafe { sys::switch_ivr_detect_speech_enable_grammar(self.as_ptr(), name.as_ptr()) };
        status_to_result(status)
    }

    /// Disables the named ASR grammar (`name`) on `session`.
    pub fn detect_speech_disable_grammar(self, name: impl AsRef<str>) -> Result<()> {
        let name = cstring(name)?;
        // SAFETY: `self.as_ptr()` is live; `name` is a valid C string for the call.
        let status =
            unsafe { sys::switch_ivr_detect_speech_disable_grammar(self.as_ptr(), name.as_ptr()) };
        status_to_result(status)
    }

    /// Disables all ASR grammars on `session`.
    pub fn detect_speech_disable_all_grammars(self) -> Result<()> {
        // SAFETY: `self.as_ptr()` is a live session.
        let status = unsafe { sys::switch_ivr_detect_speech_disable_all_grammars(self.as_ptr()) };
        status_to_result(status)
    }

    /// Starts the ASR input timers on `session` (begins the recognition timeout window).
    pub fn detect_speech_start_input_timers(self) -> Result<()> {
        // SAFETY: `self.as_ptr()` is a live session.
        let status = unsafe { sys::switch_ivr_detect_speech_start_input_timers(self.as_ptr()) };
        status_to_result(status)
    }

    /// Synthesizes and plays `text` on `session` via TTS (`switch_ivr_speak_text`). `tts_name` is
    /// the TTS engine (e.g. `"flite"`, `"cepstral"`), `voice_name` the voice, `args` an optional
    /// `switch_input_args_t*` (pass `null_mut()` for none). Interior NUL rejected.
    pub fn speak_text(
        self,
        tts_name: impl AsRef<str>,
        voice_name: impl AsRef<str>,
        text: impl AsRef<str>,
        args: *mut sys::switch_input_args_t,
    ) -> Result<()> {
        let tts = cstring(tts_name)?;
        let voice = cstring(voice_name)?;
        let text = cstring(text)?;
        // SAFETY: live session; three valid C strings; `args` is null or a valid args struct.
        status_to_result(unsafe {
            sys::switch_ivr_speak_text(
                self.as_ptr(),
                tts.as_ptr(),
                voice.as_ptr(),
                text.as_ptr(),
                args,
            )
        })
    }

    /// Holds `session`. `message` is an optional announcement string (may be empty); `moh` plays
    /// music-on-hold while held.
    pub fn hold(self, message: impl AsRef<str>, moh: bool) -> Result<()> {
        let message = cstring(message)?;
        let moh = if moh {
            sys::switch_bool_t_SWITCH_TRUE
        } else {
            sys::switch_bool_t_SWITCH_FALSE
        };
        // SAFETY: live session; valid C string; valid bool.
        status_to_result(unsafe { sys::switch_ivr_hold(self.as_ptr(), message.as_ptr(), moh) })
    }

    /// Unholds `session`.
    pub fn unhold(self) -> Result<()> {
        // SAFETY: live session.
        status_to_result(unsafe { sys::switch_ivr_unhold(self.as_ptr()) })
    }

    /// Soft-holds `session` with an `unhold_key` (DTMF that releases) and MOH for each leg.
    pub fn soft_hold(
        self,
        unhold_key: impl AsRef<str>,
        moh_a: impl AsRef<str>,
        moh_b: impl AsRef<str>,
    ) -> Result<()> {
        let k = cstring(unhold_key)?;
        let a = cstring(moh_a)?;
        let b = cstring(moh_b)?;
        // SAFETY: live session; three valid C strings.
        status_to_result(unsafe {
            sys::switch_ivr_soft_hold(self.as_ptr(), k.as_ptr(), a.as_ptr(), b.as_ptr())
        })
    }

    /// Generates/plays tones from `script` on `session`, `loops` times. `args` is an optional
    /// `switch_input_args_t*` (`null_mut()` for none).
    pub fn gentones(
        self,
        script: impl AsRef<str>,
        loops: i32,
        args: *mut sys::switch_input_args_t,
    ) -> Result<()> {
        let script = cstring(script)?;
        // SAFETY: live session; valid C string; plain int; `args` null or valid.
        status_to_result(unsafe {
            sys::switch_ivr_gentones(self.as_ptr(), script.as_ptr(), loops, args)
        })
    }

    /// Inserts `insert_file` into `file` at `sample_point` (records one into the other).
    pub fn insert_file(
        self,
        file: impl AsRef<str>,
        insert_file: impl AsRef<str>,
        sample_point: u64,
    ) -> Result<()> {
        let file = cstring(file)?;
        let ins = cstring(insert_file)?;
        // SAFETY: live session; two valid C strings; plain size.
        status_to_result(unsafe {
            sys::switch_ivr_insert_file(
                self.as_ptr(),
                file.as_ptr(),
                ins.as_ptr(),
                sample_point as sys::switch_size_t,
            )
        })
    }

    /// Enterprise-originates a new leg from `session` to `bridgeto` with a `timelimit_sec` and
    /// optional overrides. The b-leg session and cause are returned via out-params (`null_mut()`
    /// to ignore). Advanced; prefer [`originate`](Self::originate) for simple cases.
    #[allow(clippy::too_many_arguments)]
    pub fn enterprise_originate(
        self,
        bleg: *mut *mut sys::switch_core_session_t,
        cause: *mut sys::switch_call_cause_t,
        bridgeto: impl AsRef<str>,
        timelimit_sec: u32,
        table: *const sys::switch_state_handler_table_t,
        cid_name_override: *const std::os::raw::c_char,
        cid_num_override: *const std::os::raw::c_char,
        caller_profile_override: *mut sys::switch_caller_profile_t,
        ovars: *mut sys::switch_event_t,
        flags: sys::switch_originate_flag_t,
        cancel_cause: *mut sys::switch_call_cause_t,
        hl: *mut sys::switch_dial_handle_list_t,
    ) -> Result<()> {
        let bt = cstring(bridgeto)?;
        // SAFETY: live session; all args per caller contract; valid C string.
        status_to_result(unsafe {
            sys::switch_ivr_enterprise_originate(
                self.as_ptr(),
                bleg,
                cause,
                bt.as_ptr(),
                timelimit_sec,
                table,
                cid_name_override,
                cid_num_override,
                caller_profile_override,
                ovars,
                flags,
                cancel_cause,
                hl,
            )
        })
    }

    /// Enterprise-originates and bridges in one call. `data` is the dial string; `hl` a dial-handle
    /// list (may be null); `cause` out-param.
    pub fn enterprise_orig_and_bridge(
        self,
        data: impl AsRef<str>,
        hl: *mut sys::switch_dial_handle_list_t,
        cause: *mut sys::switch_call_cause_t,
    ) -> Result<()> {
        let data = cstring(data)?;
        // SAFETY: live session; valid C string; `hl`/`cause` per caller.
        status_to_result(unsafe {
            sys::switch_ivr_enterprise_orig_and_bridge(self.as_ptr(), data.as_ptr(), hl, cause)
        })
    }

    /// Originates and bridges in one call. `data` is the dial string; `dh` a dial handle (may be
    /// null); `cause` out-param.
    pub fn orig_and_bridge(
        self,
        data: impl AsRef<str>,
        dh: *mut sys::switch_dial_handle_t,
        cause: *mut sys::switch_call_cause_t,
    ) -> Result<()> {
        let data = cstring(data)?;
        // SAFETY: live session; valid C string; `dh`/`cause` per caller.
        status_to_result(unsafe {
            sys::switch_ivr_orig_and_bridge(self.as_ptr(), data.as_ptr(), dh, cause)
        })
    }

    /// Eavesdrops on the session `uuid` from `session`. `require_group` restricts to a spy group
    /// (may be empty); `flags` is a `switch_eavesdrop_flag_t` bitmask.
    pub fn eavesdrop_session(
        self,
        uuid: impl AsRef<str>,
        require_group: impl AsRef<str>,
        flags: sys::switch_eavesdrop_flag_t,
    ) -> Result<()> {
        let uuid = cstring(uuid)?;
        let group = cstring(require_group)?;
        // SAFETY: live session; two valid C strings; `flags` valid bitmask.
        status_to_result(unsafe {
            sys::switch_ivr_eavesdrop_session(self.as_ptr(), uuid.as_ptr(), group.as_ptr(), flags)
        })
    }

    /// Intercepts `uuid` onto `session`. `bleg` intercepts the b-leg too.
    pub fn intercept_session(self, uuid: impl AsRef<str>, bleg: bool) -> Result<()> {
        let uuid = cstring(uuid)?;
        let bleg = if bleg {
            sys::switch_bool_t_SWITCH_TRUE
        } else {
            sys::switch_bool_t_SWITCH_FALSE
        };
        // SAFETY: live session; valid uuid; valid bool.
        status_to_result(unsafe {
            sys::switch_ivr_intercept_session(self.as_ptr(), uuid.as_ptr(), bleg)
        })
    }

    /// Detects audio above `thresh` for `audio_hits` frames within `timeout_ms` on `session`,
    /// optionally recording to `file` (empty for none).
    pub fn detect_audio(
        self,
        thresh: u32,
        audio_hits: u32,
        timeout_ms: u32,
        file: impl AsRef<str>,
    ) -> Result<()> {
        let file = cstring(file)?;
        // SAFETY: live session; plain ints; valid C string.
        status_to_result(unsafe {
            sys::switch_ivr_detect_audio(
                self.as_ptr(),
                thresh,
                audio_hits,
                timeout_ms,
                file.as_ptr(),
            )
        })
    }

    /// Detects silence below `thresh` for `silence_hits` frames within `timeout_ms` on `session`,
    /// optionally recording to `file`.
    pub fn detect_silence(
        self,
        thresh: u32,
        silence_hits: u32,
        timeout_ms: u32,
        file: impl AsRef<str>,
    ) -> Result<()> {
        let file = cstring(file)?;
        // SAFETY: live session; plain ints; valid C string.
        status_to_result(unsafe {
            sys::switch_ivr_detect_silence(
                self.as_ptr(),
                thresh,
                silence_hits,
                timeout_ms,
                file.as_ptr(),
            )
        })
    }

    /// AEC / noise suppression / AGC via `switch_ivr_preprocess_session` (libspeexdsp).
    ///
    /// `cmds` is a FreeSWITCH preprocessor DSL with `r.` (read-leg) / `w.` (write-leg) prefix:
    /// - `echo_cancel=<tail>` — enable echo cancellation (tail in samples, default 1024);
    ///   `echo_cancel=false` disables.
    /// - `noise_suppress=<-db>` — noise suppression level (negative dB).
    /// - `echo_suppress=<-db>` — residual echo suppression.
    /// - `agc=<true|level>` — automatic gain control.
    ///
    /// Example: `"r.echo_cancel=1024 r.noise_suppress=-30 r.agc=true"`.
    pub fn preprocess_session(self, cmds: impl AsRef<str>) -> Result<()> {
        let cmds = cstring(cmds)?;
        // SAFETY: live session; valid C string. Returns switch_status_t.
        status_to_result(unsafe {
            sys::switch_ivr_preprocess_session(self.as_ptr(), cmds.as_ptr())
        })
    }

    /// Transfers `session` to `extension`/`dialplan`/`context`. Pass empty strings for defaults.
    pub fn transfer(
        self,
        extension: impl AsRef<str>,
        dialplan: impl AsRef<str>,
        context: impl AsRef<str>,
    ) -> Result<()> {
        let ext = cstring(extension)?;
        let dp = cstring(dialplan)?;
        let ctx = cstring(context)?;
        // SAFETY: live session; three valid C strings.
        status_to_result(unsafe {
            sys::switch_ivr_session_transfer(self.as_ptr(), ext.as_ptr(), dp.as_ptr(), ctx.as_ptr())
        })
    }

    /// Signal-bridges `session` and `peer_session` (no media bridge, just signalling).
    pub fn signal_bridge(self, peer_session: Session) -> Result<()> {
        // SAFETY: both sessions live.
        status_to_result(unsafe {
            sys::switch_ivr_signal_bridge(self.as_ptr(), peer_session.as_ptr())
        })
    }
}
