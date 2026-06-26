use std::ptr::NonNull;

use crate::caller::CallerProfile;
use crate::command::{borrowed_cstr_to_str, borrowed_cstr_to_string, strdup_to_string};
use crate::{Cause, Event, Result, cstring, status_to_result, sys};

/// A borrowed handle to a FreeSWITCH channel — the per-call state machine, variable store, and
/// caller-profile owner.
///
/// Obtained via [`crate::Session::channel`]. The handle borrows the session it came from and must
/// not outlive it. `Channel` is `Copy`; pass it by value.
#[derive(Copy, Clone)]
pub struct Channel {
    raw: NonNull<sys::switch_channel_t>,
}

impl Channel {
    /// Wraps a FreeSWITCH channel pointer for the duration of a callback or borrowed access.
    ///
    /// # Safety
    ///
    /// `raw` must point to a live FreeSWITCH channel and remain valid while this wrapper is used.
    pub unsafe fn from_raw(raw: *mut sys::switch_channel_t) -> Option<Self> {
        NonNull::new(raw).map(|raw| Self { raw })
    }

    #[inline]
    pub fn as_ptr(self) -> *mut sys::switch_channel_t {
        self.raw.as_ptr()
    }

    /// Reads a channel variable into an owned `String`.
    ///
    /// Uses `switch_channel_get_variable_strdup`, which returns a freshly malloc'd copy (no memory
    /// pool) that this method frees after copying. The result does not borrow the channel and is not
    /// invalidated by later `set_variable` calls. Returns `Ok(None)` when the variable is unset.
    pub fn variable(self, name: impl AsRef<str>) -> Result<Option<String>> {
        let name = cstring(name)?;
        // SAFETY: `self.raw` is a live channel; `name` is a valid C string for the call. The
        // returned pointer is null or a malloc'd "strdup copy ... without using a memory pool"
        // (per switch_channel.h) that `strdup_to_string` copies out and frees.
        let value =
            unsafe { sys::switch_channel_get_variable_strdup(self.raw.as_ptr(), name.as_ptr()) };
        // SAFETY: `value` is null or a malloc'd C string as above.
        Ok(unsafe { strdup_to_string(value.cast_mut()) })
    }

    /// Sets a channel variable, substituting it into the channel's variable scope.
    pub fn set_variable(self, name: impl AsRef<str>, value: &str) -> Result<()> {
        let name = cstring(name)?;
        let value = cstring(value)?;
        // SAFETY: `self.raw` is a live channel; both C strings are valid for the call.
        let status = unsafe {
            sys::switch_channel_set_variable_var_check(
                self.raw.as_ptr(),
                name.as_ptr(),
                value.as_ptr(),
                sys::switch_bool_t_SWITCH_TRUE,
            )
        };
        status_to_result(status)
    }

    /// The channel's display name (e.g. `"sofia/internal/1001@..."`).
    pub fn name(self) -> Option<String> {
        // SAFETY: `self.raw` is a live channel.
        let ptr = unsafe { sys::switch_channel_get_name(self.raw.as_ptr()) };
        borrowed_cstr_to_string(ptr.cast_const())
    }

    /// The channel's UUID.
    pub fn uuid(self) -> Option<String> {
        // SAFETY: `self.raw` is a live channel.
        let ptr = unsafe { sys::switch_channel_get_uuid(self.raw.as_ptr()) };
        borrowed_cstr_to_string(ptr.cast_const())
    }

    /// The channel's current state (`CS_*`).
    pub fn state(self) -> sys::switch_channel_state_t {
        // SAFETY: `self.raw` is a live channel.
        unsafe { sys::switch_channel_get_state(self.raw.as_ptr()) }
    }

    /// The hangup cause recorded on the channel.
    pub fn cause(self) -> Cause {
        // SAFETY: `self.raw` is a live channel.
        unsafe { sys::switch_channel_get_cause(self.raw.as_ptr()) }
    }

    /// The caller profile attached to this channel.
    pub fn caller_profile(self) -> Option<CallerProfile> {
        // SAFETY: `self.raw` is a live channel.
        let raw = unsafe { sys::switch_channel_get_caller_profile(self.raw.as_ptr()) };
        // SAFETY: The profile borrows the channel and is live while `self` is.
        unsafe { CallerProfile::from_raw(raw) }
    }

    /// Returns `true` when `flag` (`CF_*`) is set on the channel.
    pub fn test_flag(self, flag: sys::switch_channel_flag_t) -> bool {
        // SAFETY: `self.raw` is a live channel.
        let set = unsafe { sys::switch_channel_test_flag(self.raw.as_ptr(), flag) };
        set != 0
    }

    /// Blocks the caller until the channel reaches `want` state. A null `other_channel` is passed so
    /// only this channel's own state is awaited.
    pub fn wait_for_state(self, want: sys::switch_channel_state_t) {
        // SAFETY: `self.raw` is a live channel; a null `other_channel` is permitted.
        unsafe {
            sys::switch_channel_wait_for_state(self.raw.as_ptr(), std::ptr::null_mut(), want)
        };
    }

    /// Requests a state transition. Returns the resulting state.
    pub fn set_state(self, state: sys::switch_channel_state_t) -> sys::switch_channel_state_t {
        // SAFETY: `self.raw` is a live channel; source strings are static C strings.
        unsafe {
            sys::switch_channel_perform_set_state(
                self.raw.as_ptr(),
                c"fswtch-rs".as_ptr(),
                c"Channel::set_state".as_ptr(),
                line!() as _,
                state,
            )
        }
    }

    /// Hangs up the channel with the given cause. Returns the resulting state.
    pub fn hangup(self, cause: Cause) -> sys::switch_channel_state_t {
        // SAFETY: `self.raw` is a live channel; source strings are static C strings.
        unsafe {
            sys::switch_channel_perform_hangup(
                self.raw.as_ptr(),
                c"fswtch-rs".as_ptr(),
                c"Channel::hangup".as_ptr(),
                line!() as _,
                cause,
            )
        }
    }

    // ----- State / call-state / direction / timing ---------------------------

    /// The channel's "running" state — the state the state machine is currently executing, which may
    /// lag behind [`state`](Self::state) during a transition.
    pub fn running_state(self) -> sys::switch_channel_state_t {
        // SAFETY: `self.raw` is a live channel.
        unsafe { sys::switch_channel_get_running_state(self.raw.as_ptr()) }
    }

    /// The channel's call-state (`CC_*`), a finer-grained call-progress view than
    /// [`state`](Self::state).
    pub fn callstate(self) -> sys::switch_channel_callstate_t {
        // SAFETY: `self.raw` is a live channel.
        unsafe { sys::switch_channel_get_callstate(self.raw.as_ptr()) }
    }

    /// Sets a state flag (`CF_*`) on the channel without transitioning state.
    pub fn set_state_flag(self, flag: sys::switch_channel_flag_t) {
        // SAFETY: `self.raw` is a live channel.
        unsafe { sys::switch_channel_set_state_flag(self.raw.as_ptr(), flag) };
    }

    /// Registers a state-handler table for this channel. Returns the number of state handlers now
    /// registered.
    ///
    /// `table` is a raw pointer to a `switch_state_handler_table_t` (typically produced by a
    /// module's state-handler constructor). It must remain valid for the channel's lifetime.
    pub fn add_state_handler(self, table: *const sys::switch_state_handler_table_t) -> i32 {
        // SAFETY: `self.raw` is a live channel; `table` is null or a valid handler table per the
        // caller's contract.
        unsafe { sys::switch_channel_add_state_handler(self.raw.as_ptr(), table) }
    }

    /// Removes a previously-registered state-handler table from this channel.
    ///
    /// `table` must point to the same table earlier passed to [`add_state_handler`](Self::add_state_handler).
    pub fn clear_state_handler(self, table: *const sys::switch_state_handler_table_t) {
        // SAFETY: `self.raw` is a live channel; `table` is null or a previously-registered table.
        unsafe { sys::switch_channel_clear_state_handler(self.raw.as_ptr(), table) };
    }

    /// Pokes the channel's state thread so it re-checks pending signals. When `in_thread_only` is
    /// true the check is confined to the state thread without locking. Returns a non-zero value when
    /// a signal was detected.
    pub fn check_signal(self, in_thread_only: bool) -> i32 {
        // SAFETY: `self.raw` is a live channel.
        unsafe {
            sys::switch_channel_check_signal(
                self.raw.as_ptr(),
                if in_thread_only {
                    sys::switch_bool_t_SWITCH_TRUE
                } else {
                    sys::switch_bool_t_SWITCH_FALSE
                },
            )
        }
    }

    /// The channel's event/timing table (created/answered/bridged/hungup timestamps). The returned
    /// pointer borrows the channel's storage and is invalidated by state transitions; read it and
    /// drop the reference before driving the channel further.
    ///
    /// Returns `None` when no timetable is attached.
    ///
    /// # Safety escape hatch
    ///
    /// The raw pointer is not wrapped in a safe type because `switch_channel_timetable_t` is an
    /// opaque C struct with public fields; dereference it only while this `Channel` is live.
    pub fn timetable(self) -> Option<*mut sys::switch_channel_timetable_t> {
        // SAFETY: `self.raw` is a live channel; the returned pointer borrows channel storage.
        let ptr = unsafe { sys::switch_channel_get_timetable(self.raw.as_ptr()) };
        if ptr.is_null() { None } else { Some(ptr) }
    }

    /// The logical call direction of this channel (`SWITCH_CALL_DIRECTION_*`).
    pub fn direction(self) -> sys::switch_call_direction_t {
        // SAFETY: `self.raw` is a live channel.
        unsafe { sys::switch_channel_direction(self.raw.as_ptr()) }
    }

    /// Overrides the channel's call direction (`SWITCH_CALL_DIRECTION_*`).
    pub fn set_direction(self, direction: sys::switch_call_direction_t) {
        // SAFETY: `self.raw` is a live channel.
        unsafe { sys::switch_channel_set_direction(self.raw.as_ptr(), direction) };
    }

    /// Records the current wall-clock time onto the channel's timetable as the bridge timestamp.
    /// (The C function returns void, so this is infallible.)
    pub fn set_bridge_time(self) -> Result<()> {
        // SAFETY: `self.raw` is a live channel; the call returns void.
        unsafe { sys::switch_channel_set_bridge_time(self.raw.as_ptr()) };
        Ok(())
    }

    /// Records the current wall-clock time onto the channel's timetable as the hangup timestamp.
    pub fn set_hangup_time(self) {
        // SAFETY: `self.raw` is a live channel.
        unsafe { sys::switch_channel_set_hangup_time(self.raw.as_ptr()) };
    }

    /// Finalises the channel's timestamps (created/answered/progress/bridged/hungup) onto its
    /// timetable. Returns `Err` when the channel has no timetable.
    pub fn set_timestamps(self) -> Result<()> {
        // SAFETY: `self.raw` is a live channel.
        let status = unsafe { sys::switch_channel_set_timestamps(self.raw.as_ptr()) };
        status_to_result(status)
    }

    /// The device record bound to this channel, if any. The pointer borrows channel storage; release
    /// it with [`switch_channel_release_device_record`] when done (or simply drop the reference).
    ///
    /// # Safety escape hatch
    ///
    /// Raw pointer to an opaque C struct; dereference only while this `Channel` is live.
    pub fn device_record(self) -> Option<*mut sys::switch_device_record_t> {
        // SAFETY: `self.raw` is a live channel; the returned pointer borrows channel storage.
        let ptr = unsafe { sys::switch_channel_get_device_record(self.raw.as_ptr()) };
        if ptr.is_null() { None } else { Some(ptr) }
    }

    /// Clears (dereferences) the device record bound to this channel.
    pub fn clear_device_record(self) {
        // SAFETY: `self.raw` is a live channel.
        unsafe { sys::switch_channel_clear_device_record(self.raw.as_ptr()) };
    }

    // ----- DTMF --------------------------------------------------------------

    /// Dequeues one DTMF event from the channel's DTMF buffer into `dtmf`. Returns `Ok(())` when an
    /// event was dequeued, or `Err` when the queue was empty.
    pub fn dequeue_dtmf(self, dtmf: &mut crate::Dtmf) -> Result<()> {
        // SAFETY: `self.raw` is a live channel; `dtmf` is a uniquely-borrowed `switch_dtmf_t` whose
        // `&mut` gives us write access for the duration of the call. Casting the const pointer to
        // mut is sound because the `&mut Dtmf` borrow guarantees no aliasing.
        let status =
            unsafe { sys::switch_channel_dequeue_dtmf(self.raw.as_ptr(), dtmf.as_ptr() as *mut _) };
        status_to_result(status)
    }

    /// Dequeues all pending DTMF from the channel into `buf` as a string (e.g. `"123#"`). Returns
    /// the number of bytes written (excluding the NUL terminator), which is also the length of the
    /// appended portion of `buf`. `buf` is NUL-terminated in place.
    pub fn dequeue_dtmf_string(self, buf: &mut [u8]) -> usize {
        // SAFETY: `self.raw` is a live channel; `buf` is a valid writable byte slice for the
        // duration of the call. `switch_size_t` is `usize`.
        let written = unsafe {
            sys::switch_channel_dequeue_dtmf_string(
                self.raw.as_ptr(),
                buf.as_mut_ptr().cast(),
                buf.len(),
            )
        };
        written as usize
    }

    /// Discards all queued DTMF on the channel.
    pub fn flush_dtmf(self) {
        // SAFETY: `self.raw` is a live channel.
        unsafe { sys::switch_channel_flush_dtmf(self.raw.as_ptr()) };
    }

    /// Acquires the channel's DTMF mutex. Returns `Err` on failure.
    pub fn dtmf_lock(self) -> Result<()> {
        // SAFETY: `self.raw` is a live channel.
        let status = unsafe { sys::switch_channel_dtmf_lock(self.raw.as_ptr()) };
        status_to_result(status)
    }

    /// Releases the channel's DTMF mutex. Returns `Err` on failure.
    pub fn dtmf_unlock(self) -> Result<()> {
        // SAFETY: `self.raw` is a live channel.
        let status = unsafe { sys::switch_channel_dtmf_unlock(self.raw.as_ptr()) };
        status_to_result(status)
    }

    // ----- Event data --------------------------------------------------------

    /// Populates `event` with the channel's basic data (name, uuid, state, direction, caller id,
    /// ...). Use this as the first of the three `event_set_*_data` calls when serialising a channel
    /// into a [`Event`].
    pub fn event_set_basic_data(self, event: &Event) {
        // SAFETY: `self.raw` is a live channel; `event.as_ptr()` is a live event pointer borrowed
        // for the duration of this call.
        unsafe { sys::switch_channel_event_set_basic_data(self.raw.as_ptr(), event.as_ptr()) };
    }

    /// Populates `event` with the channel's standard data (variables, caller profile fields, ...).
    /// Usually called after [`event_set_basic_data`](Self::event_set_basic_data).
    pub fn event_set_data(self, event: &Event) {
        // SAFETY: `self.raw` is a live channel; `event.as_ptr()` is a live event pointer.
        unsafe { sys::switch_channel_event_set_data(self.raw.as_ptr(), event.as_ptr()) };
    }

    /// Populates `event` with the channel's extended data (app flags, hold music, partner uuid,
    /// ...). Usually called after [`event_set_data`](Self::event_set_data).
    pub fn event_set_extended_data(self, event: &Event) {
        // SAFETY: `self.raw` is a live channel; `event.as_ptr()` is a live event pointer.
        unsafe { sys::switch_channel_event_set_extended_data(self.raw.as_ptr(), event.as_ptr()) };
    }

    /// Builds a newline-separated parameter string from this channel's caller profile (and, when
    /// given, `caller_profile` overrides). The returned string borrows the channel's memory pool
    /// storage and is invalidated by further channel mutations; copy it out if you need it to last.
    ///
    /// `prefix` is prepended to each parameter name (pass `""` for none). Pass `None` for
    /// `caller_profile` to use the channel's own profile.
    pub fn build_param_string(
        self,
        caller_profile: Option<&CallerProfile>,
        prefix: impl AsRef<str>,
    ) -> Result<Option<String>> {
        let prefix = cstring(prefix)?;
        let profile_ptr = caller_profile
            .map(|p| p.as_ptr())
            .unwrap_or(std::ptr::null_mut());
        // SAFETY: `self.raw` is a live channel; `prefix` is a valid C string; `profile_ptr` is null
        // or a valid caller-profile pointer. The returned pointer borrows channel pool storage and
        // is read (and copied out) before any further channel mutation in this call.
        let ptr = unsafe {
            sys::switch_channel_build_param_string(self.raw.as_ptr(), profile_ptr, prefix.as_ptr())
        };
        // SAFETY: `ptr` is null or a C string borrowed from the channel pool (valid for the duration
        // of this call, before the channel is mutated further). Read-only copy out.
        Ok(borrowed_cstr_to_string(ptr.cast_const()))
    }

    // ----- Caller profile ----------------------------------------------------

    /// Attaches a caller profile to this channel, replacing any existing one.
    pub fn set_caller_profile(self, profile: &CallerProfile) {
        // SAFETY: `self.raw` is a live channel; `profile.as_ptr()` is a valid caller-profile pointer.
        unsafe { sys::switch_channel_set_caller_profile(self.raw.as_ptr(), profile.as_ptr()) };
    }

    /// Increments the caller-profile step counter (used to invalidate cached profile views).
    pub fn step_caller_profile(self) {
        // SAFETY: `self.raw` is a live channel.
        unsafe { sys::switch_channel_step_caller_profile(self.raw.as_ptr()) };
    }

    /// Attaches an "origination" caller profile to this channel (the profile of the leg that
    /// originated the call).
    pub fn set_origination_caller_profile(self, profile: &CallerProfile) {
        // SAFETY: `self.raw` is a live channel; `profile.as_ptr()` is a valid caller-profile pointer.
        unsafe {
            sys::switch_channel_set_origination_caller_profile(self.raw.as_ptr(), profile.as_ptr())
        };
    }

    /// The "origination" caller profile attached to this channel, if any.
    pub fn origination_caller_profile(self) -> Option<CallerProfile> {
        // SAFETY: `self.raw` is a live channel.
        let raw = unsafe { sys::switch_channel_get_origination_caller_profile(self.raw.as_ptr()) };
        // SAFETY: The profile borrows the channel and is live while `self` is.
        unsafe { CallerProfile::from_raw(raw) }
    }

    /// Attaches an "originator" caller profile to this channel (the profile of the calling leg in a
    /// bridge).
    pub fn set_originator_caller_profile(self, profile: &CallerProfile) {
        // SAFETY: `self.raw` is a live channel; `profile.as_ptr()` is a valid caller-profile pointer.
        unsafe {
            sys::switch_channel_set_originator_caller_profile(self.raw.as_ptr(), profile.as_ptr())
        };
    }

    /// The "originator" caller profile attached to this channel, if any.
    pub fn originator_caller_profile(self) -> Option<CallerProfile> {
        // SAFETY: `self.raw` is a live channel.
        let raw = unsafe { sys::switch_channel_get_originator_caller_profile(self.raw.as_ptr()) };
        // SAFETY: The profile borrows the channel and is live while `self` is.
        unsafe { CallerProfile::from_raw(raw) }
    }

    /// Attaches an "originatee" caller profile to this channel (the profile of the called leg in a
    /// bridge).
    pub fn set_originatee_caller_profile(self, profile: &CallerProfile) {
        // SAFETY: `self.raw` is a live channel; `profile.as_ptr()` is a valid caller-profile pointer.
        unsafe {
            sys::switch_channel_set_originatee_caller_profile(self.raw.as_ptr(), profile.as_ptr())
        };
    }

    /// The "originatee" caller profile attached to this channel, if any.
    pub fn originatee_caller_profile(self) -> Option<CallerProfile> {
        // SAFETY: `self.raw` is a live channel.
        let raw = unsafe { sys::switch_channel_get_originatee_caller_profile(self.raw.as_ptr()) };
        // SAFETY: The profile borrows the channel and is live while `self` is.
        unsafe { CallerProfile::from_raw(raw) }
    }

    /// Attaches a caller extension to this channel. `extension` is a raw pointer because the crate
    /// does not yet wrap `switch_caller_extension_t`; it must point to a live caller extension.
    ///
    /// # Safety escape hatch
    ///
    /// `extension` must be a valid `switch_caller_extension_t` pointer that outlives this call.
    pub fn set_caller_extension(self, extension: *mut sys::switch_caller_extension_t) {
        // SAFETY: `self.raw` is a live channel; the caller guarantees `extension` is valid.
        unsafe { sys::switch_channel_set_caller_extension(self.raw.as_ptr(), extension) };
    }

    /// The caller extension attached to this channel, if any.
    ///
    /// # Safety escape hatch
    ///
    /// Raw pointer to an opaque C struct (`switch_caller_extension_t`); dereference only while this
    /// `Channel` is live.
    pub fn caller_extension(self) -> Option<*mut sys::switch_caller_extension_t> {
        // SAFETY: `self.raw` is a live channel; the returned pointer borrows channel storage.
        let raw = unsafe { sys::switch_channel_get_caller_extension(self.raw.as_ptr()) };
        if raw.is_null() { None } else { Some(raw) }
    }

    /// The queued caller extension (an extension parked for later execution) on this channel, if
    /// any.
    ///
    /// # Safety escape hatch
    ///
    /// Raw pointer to an opaque C struct (`switch_caller_extension_t`); dereference only while this
    /// `Channel` is live.
    pub fn queued_extension(self) -> Option<*mut sys::switch_caller_extension_t> {
        // SAFETY: `self.raw` is a live channel; the returned pointer borrows channel storage.
        let raw = unsafe { sys::switch_channel_get_queued_extension(self.raw.as_ptr()) };
        if raw.is_null() { None } else { Some(raw) }
    }

    /// Masquerades the caller extensions of `orig_channel` onto `new_channel` starting at `offset`.
    /// Returns `Err` on failure. This is a static method because it operates on two channels.
    pub fn caller_extension_masquerade(orig: Channel, new: Channel, offset: u32) -> Result<()> {
        // SAFETY: both channels are live; `offset` is an index into the extension's application
        // list.
        let status = unsafe {
            sys::switch_channel_caller_extension_masquerade(orig.as_ptr(), new.as_ptr(), offset)
        };
        status_to_result(status)
    }

    // ----- Caller id / hold music / partner / misc ---------------------------

    /// Flips the caller-id name and number on this channel (swaps `caller_id_name` and
    /// `caller_id_number`).
    pub fn flip_cid(self) {
        // SAFETY: `self.raw` is a live channel.
        unsafe { sys::switch_channel_flip_cid(self.raw.as_ptr()) };
    }

    /// The hold-music URI configured for this channel (borrowed from channel storage).
    pub fn hold_music(self) -> Option<&'static str> {
        // SAFETY: `self.raw` is a live channel; the returned pointer borrows channel storage. We tie
        // the lifetime to `&'static` because the value is typically a static or pool string that
        // outlives the borrow; callers should treat it as borrowed from the channel.
        let ptr = unsafe { sys::switch_channel_get_hold_music(self.raw.as_ptr()) };
        // SAFETY: `ptr` is null or a borrowed C string valid for the duration of this call.
        unsafe { borrowed_cstr_to_str(ptr) }
    }

    /// The hold-music URI configured for this channel's bridge partner (borrowed from channel
    /// storage).
    pub fn hold_music_partner(self) -> Option<&'static str> {
        // SAFETY: `self.raw` is a live channel.
        let ptr = unsafe { sys::switch_channel_get_hold_music_partner(self.raw.as_ptr()) };
        // SAFETY: `ptr` is null or a borrowed C string.
        unsafe { borrowed_cstr_to_str(ptr) }
    }

    /// The head of this channel's hold-record linked list, if any.
    ///
    /// # Safety escape hatch
    ///
    /// Raw pointer to an opaque C struct (`switch_hold_record_t`); dereference only while this
    /// `Channel` is live.
    pub fn hold_record(self) -> Option<*mut sys::switch_hold_record_t> {
        // SAFETY: `self.raw` is a live channel; the returned pointer borrows channel storage.
        let ptr = unsafe { sys::switch_channel_get_hold_record(self.raw.as_ptr()) };
        if ptr.is_null() { None } else { Some(ptr) }
    }

    /// The channel's log-tag event handle (an event populated with logging tags). Returns `Ok(Some`
    /// raw pointer `))` when tags are present.
    ///
    /// # Safety escape hatch
    ///
    /// The returned pointer is a raw `*mut switch_event_t` (the crate does not wrap a borrowed
    /// event here); it borrows channel storage. Dereference only while this `Channel` is live.
    pub fn log_tags(self) -> Option<*mut sys::switch_event_t> {
        let mut out: *mut sys::switch_event_t = std::ptr::null_mut();
        // SAFETY: `self.raw` is a live channel; `out` is a valid write slot.
        let status = unsafe { sys::switch_channel_get_log_tags(self.raw.as_ptr(), &mut out) };
        if status_to_result(status).is_ok() && !out.is_null() {
            Some(out)
        } else {
            None
        }
    }

    /// Sets presence-data column values from a comma-separated `cols` string (e.g.
    /// `"user,host,domain"`).
    pub fn set_presence_data_vals(self, cols: impl AsRef<str>) -> Result<()> {
        let cols = cstring(cols)?;
        // SAFETY: `self.raw` is a live channel; `cols` is a valid C string.
        unsafe { sys::switch_channel_set_presence_data_vals(self.raw.as_ptr(), cols.as_ptr()) };
        Ok(())
    }

    /// The partner channel's UUID (borrowed from channel storage). Returns `None` when the channel
    /// is not bridged.
    pub fn partner_uuid(self) -> Option<&'static str> {
        // SAFETY: `self.raw` is a live channel.
        let ptr = unsafe { sys::switch_channel_get_partner_uuid(self.raw.as_ptr()) };
        // SAFETY: `ptr` is null or a borrowed C string.
        unsafe { borrowed_cstr_to_str(ptr) }
    }

    /// Copies the partner channel's UUID into `buf`, returning the number of bytes written
    /// (excluding the NUL). Returns `Ok(None)` when the channel is not bridged or the buffer was too
    /// small. `buf` is NUL-terminated in place.
    pub fn partner_uuid_copy(self, buf: &mut [u8]) -> Option<usize> {
        use std::ffi::CStr;
        // SAFETY: `self.raw` is a live channel; `buf` is a valid writable byte slice for the
        // duration of the call.
        let ret = unsafe {
            sys::switch_channel_get_partner_uuid_copy(
                self.raw.as_ptr(),
                buf.as_mut_ptr().cast(),
                buf.len(),
            )
        };
        // `switch_channel_get_partner_uuid_copy` returns null when there is no partner (or the
        // buffer is too small) and the buffer pointer otherwise.
        if ret.is_null() {
            None
        } else {
            // SAFETY: `ret` is the in-place NUL-terminated buffer we just wrote into `buf`.
            let len = unsafe { CStr::from_ptr(ret) }.to_bytes().len();
            Some(len)
        }
    }

    /// Clears the per-app flag bitset registered under `app` on this channel.
    pub fn clear_app_flag_key(self, app: impl AsRef<str>, flags: u32) -> Result<()> {
        let app = cstring(app)?;
        // SAFETY: `self.raw` is a live channel; `app` is a valid C string. Note the C signature
        // order is `(app, channel, flags)`.
        unsafe { sys::switch_channel_clear_app_flag_key(app.as_ptr(), self.raw.as_ptr(), flags) };
        Ok(())
    }

    // ----- Variables (extended) ----------------------------------------------

    /// Reads a channel variable, optionally duplicating it into the channel's memory pool.
    ///
    /// Wraps `switch_channel_get_variable_dup` with `idx = 0`. When `dup` is `false` the returned
    /// string borrows the channel's pool storage and is tied to `&self`; when `dup` is `true`
    /// FreeSWITCH duplicates the value into the pool (still tied to the channel, still no free).
    /// Returns `Ok(None)` when the variable is unset.
    pub fn variable_dup<'a>(self, name: impl AsRef<str>, dup: bool) -> Result<Option<&'a str>> {
        let name = cstring(name)?;
        let dup = if dup {
            sys::switch_bool_t_SWITCH_TRUE
        } else {
            sys::switch_bool_t_SWITCH_FALSE
        };
        // SAFETY: `self.raw` is a live channel; `name` is a valid C string. The returned pointer
        // is null or a string stored in the channel's memory pool (valid while `self` is live).
        let ptr = unsafe {
            sys::switch_channel_get_variable_dup(self.raw.as_ptr(), name.as_ptr(), dup, 0)
        };
        // SAFETY: `ptr` is null or a pool-backed C string valid for the lifetime of `&self`.
        Ok(unsafe { borrowed_cstr_to_str(ptr) })
    }

    /// Reads a variable from the channel's peer/bridged channel.
    ///
    /// The returned string borrows the peer channel's pool storage and is tied to `&self`.
    /// Returns `Ok(None)` when unset.
    pub fn variable_partner<'a>(self, name: impl AsRef<str>) -> Result<Option<&'a str>> {
        let name = cstring(name)?;
        // SAFETY: `self.raw` is a live channel; `name` is a valid C string. The returned pointer
        // is null or a string stored in pool storage tied to the peer channel's lifetime.
        let ptr =
            unsafe { sys::switch_channel_get_variable_partner(self.raw.as_ptr(), name.as_ptr()) };
        // SAFETY: `ptr` is null or a pool-backed C string valid for the duration of this call.
        Ok(unsafe { borrowed_cstr_to_str(ptr) })
    }

    /// Reads a channel variable into a caller-supplied byte buffer.
    ///
    /// Wraps `switch_channel_get_variable_buf`, which copies up to `buf.len() - 1` bytes (NUL
    /// terminated) into `buf`. The returned `usize` is the number of bytes written (excluding the
    /// NUL terminator). Returns an error when the variable is unset or the buffer is too small.
    pub fn variable_buf(self, name: impl AsRef<str>, buf: &mut [u8]) -> Result<usize> {
        let name = cstring(name)?;
        let len = buf.len();
        // SAFETY: `self.raw` is a live channel; `name` is a valid C string; `buf` is a writable
        // region of `len` bytes borrowed for the duration of the call.
        let status = unsafe {
            sys::switch_channel_get_variable_buf(
                self.raw.as_ptr(),
                name.as_ptr(),
                buf.as_mut_ptr().cast(),
                len as sys::switch_size_t,
            )
        };
        if status != crate::SUCCESS {
            return Err(crate::SwitchError(status));
        }
        // The C function NUL-terminates; find the end of the written C string.
        let written = buf.iter().position(|&b| b == 0).unwrap_or(len);
        Ok(written)
    }

    /// Snapshots every variable on the channel into an owned `Vec` of `(name, value)` pairs.
    ///
    /// Wraps `switch_channel_get_variables`, which builds a temporary `switch_event` (destroyed
    /// before this method returns) whose headers are copied out. The returned pairs own their
    /// storage and do not borrow the channel.
    pub fn variables(self) -> Result<Vec<(String, String)>> {
        // SAFETY: closure captures `self` (a live channel) and the null out-param; FreeSWITCH
        // populates it on success and we destroy it before returning.
        collect_channel_variables(|ev| unsafe {
            sys::switch_channel_get_variables(self.raw.as_ptr(), ev)
        })
    }

    /// Snapshots every variable whose name starts with `prefix` into an owned `Vec` of
    /// `(name, value)` pairs.
    ///
    /// Wraps `switch_channel_get_variables_prefix`. The temporary event is destroyed before this
    /// method returns and the pairs own their storage.
    pub fn variables_prefix(self, prefix: impl AsRef<str>) -> Result<Vec<(String, String)>> {
        let prefix = cstring(prefix)?;
        // SAFETY: `self.raw` is a live channel; `prefix` is a valid C string; the out-param is
        // populated on success and destroyed below.
        collect_channel_variables(|ev| unsafe {
            sys::switch_channel_get_variables_prefix(self.raw.as_ptr(), prefix.as_ptr(), ev)
        })
    }

    /// Snapshots the channel's scope variables into an owned `Vec` of `(name, value)` pairs.
    ///
    /// Wraps `switch_channel_get_scope_variables`. The temporary event is destroyed before this
    /// method returns; the returned pairs own their storage.
    pub fn scope_variables(self) -> Result<Vec<(String, String)>> {
        // SAFETY: `self.raw` is a live channel; the out-param is populated on success and destroyed
        // below.
        collect_channel_variables(|ev| unsafe {
            sys::switch_channel_get_scope_variables(self.raw.as_ptr(), ev)
        })
    }

    /// Appends (or prepends) a value to a multi-valued channel variable.
    ///
    /// Wraps `switch_channel_add_variable_var_check` with `SWITCH_TRUE` (run the variable check).
    /// `stack` selects whether the value is pushed to the bottom (`SWITCH_STACK_BOTTOM`) or top
    /// (`SWITCH_STACK_TOP`) of the existing values.
    pub fn add_variable(
        self,
        name: impl AsRef<str>,
        value: &str,
        stack: sys::switch_stack_t,
    ) -> Result<()> {
        let name = cstring(name)?;
        let value = cstring(value)?;
        // SAFETY: `self.raw` is a live channel; both C strings are valid for the call.
        let status = unsafe {
            sys::switch_channel_add_variable_var_check(
                self.raw.as_ptr(),
                name.as_ptr(),
                value.as_ptr(),
                sys::switch_bool_t_SWITCH_TRUE,
                stack,
            )
        };
        status_to_result(status)
    }

    /// Removes every variable whose name starts with `prefix`. Returns the count removed.
    pub fn del_variable_prefix(self, prefix: impl AsRef<str>) -> Result<u32> {
        let prefix = cstring(prefix)?;
        // SAFETY: `self.raw` is a live channel; `prefix` is a valid C string.
        let removed =
            unsafe { sys::switch_channel_del_variable_prefix(self.raw.as_ptr(), prefix.as_ptr()) };
        Ok(removed)
    }

    /// Exports a channel variable to the peer channel under `export_varname`.
    ///
    /// Wraps `switch_channel_export_variable_var_check` with `SWITCH_TRUE` (run the variable
    /// check). When `export_varname` is `None` the original `varname` is used on the peer.
    pub fn export_variable(
        self,
        varname: impl AsRef<str>,
        value: &str,
        export_varname: Option<impl AsRef<str>>,
    ) -> Result<()> {
        let varname = cstring(varname)?;
        let value = cstring(value)?;
        let export_varname = export_varname.map(cstring).transpose()?;
        let export_ptr = export_varname
            .as_ref()
            .map(|c| c.as_ptr())
            .unwrap_or_else(std::ptr::null);
        // SAFETY: `self.raw` is a live channel; all non-null C strings are valid for the call.
        let status = unsafe {
            sys::switch_channel_export_variable_var_check(
                self.raw.as_ptr(),
                varname.as_ptr(),
                value.as_ptr(),
                export_ptr,
                sys::switch_bool_t_SWITCH_TRUE,
            )
        };
        status_to_result(status)
    }

    /// Sets a variable on the channel's peer/bridged channel.
    ///
    /// Wraps `switch_channel_set_variable_partner_var_check` with `SWITCH_TRUE`.
    pub fn set_variable_partner(self, name: impl AsRef<str>, value: &str) -> Result<()> {
        let name = cstring(name)?;
        let value = cstring(value)?;
        // SAFETY: `self.raw` is a live channel; both C strings are valid for the call.
        let status = unsafe {
            sys::switch_channel_set_variable_partner_var_check(
                self.raw.as_ptr(),
                name.as_ptr(),
                value.as_ptr(),
                sys::switch_bool_t_SWITCH_TRUE,
            )
        };
        status_to_result(status)
    }

    /// Sets a variable, stripping any surrounding double-quotes from `value` first.
    ///
    /// Wraps `switch_channel_set_variable_strip_quotes_var_check` with `SWITCH_TRUE`.
    pub fn set_variable_strip_quotes(self, name: impl AsRef<str>, value: &str) -> Result<()> {
        let name = cstring(name)?;
        let value = cstring(value)?;
        // SAFETY: `self.raw` is a live channel; both C strings are valid for the call.
        let status = unsafe {
            sys::switch_channel_set_variable_strip_quotes_var_check(
                self.raw.as_ptr(),
                name.as_ptr(),
                value.as_ptr(),
                sys::switch_bool_t_SWITCH_TRUE,
            )
        };
        status_to_result(status)
    }

    /// Expands `${...}` / `$$` variable references in `input` against this channel's variable
    /// scope.
    ///
    /// Wraps `switch_channel_expand_variables_check` with no var/api list and `recur = 0`. The
    /// returned string is freshly `malloc`'d by FreeSWITCH; this method copies it out and frees
    /// the original, so the result owns its storage and does not borrow the channel.
    pub fn expand_variables(self, input: impl AsRef<str>) -> Result<Option<String>> {
        let input = cstring(input)?;
        // SAFETY: `self.raw` is a live channel; `input` is a valid C string. The returned pointer
        // is null or a malloc'd C string the caller must free.
        let ptr = unsafe {
            sys::switch_channel_expand_variables_check(
                self.raw.as_ptr(),
                input.as_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
            )
        };
        // SAFETY: `ptr` is null or a malloc'd C string per the call contract.
        Ok(unsafe { strdup_to_string(ptr) })
    }

    /// Runs the `execute_on_<prefix>` applications configured on this channel.
    ///
    /// Wraps `switch_channel_execute_on`. `variable_prefix` is the prefix to match (e.g.
    /// `"execute_on_answer"`).
    pub fn execute_on(self, variable_prefix: impl AsRef<str>) -> Result<()> {
        let prefix = cstring(variable_prefix)?;
        // SAFETY: `self.raw` is a live channel; `prefix` is a valid C string.
        let status = unsafe { sys::switch_channel_execute_on(self.raw.as_ptr(), prefix.as_ptr()) };
        status_to_result(status)
    }

    /// Runs the `execute_on` applications matching an explicit variable value.
    ///
    /// Wraps `switch_channel_execute_on_value`. `variable_value` is the exact value to match.
    pub fn execute_on_value(self, variable_value: impl AsRef<str>) -> Result<()> {
        let value = cstring(variable_value)?;
        // SAFETY: `self.raw` is a live channel; `value` is a valid C string.
        let status =
            unsafe { sys::switch_channel_execute_on_value(self.raw.as_ptr(), value.as_ptr()) };
        status_to_result(status)
    }

    /// Fires the `api_on_<prefix>` API commands configured on this channel.
    ///
    /// Wraps `switch_channel_api_on`. `variable_prefix` is the prefix to match (e.g.
    /// `"api_on_answer"`).
    pub fn api_on(self, variable_prefix: impl AsRef<str>) -> Result<()> {
        let prefix = cstring(variable_prefix)?;
        // SAFETY: `self.raw` is a live channel; `prefix` is a valid C string.
        let status = unsafe { sys::switch_channel_api_on(self.raw.as_ptr(), prefix.as_ptr()) };
        status_to_result(status)
    }

    // ----- Flags ---------------------------------------------------------------

    /// Sets a channel flag (`CF_*`) with an explicit integer value.
    ///
    /// Wraps `switch_channel_set_flag_value`. FreeSWITCH exposes no zero-argument
    /// `switch_channel_set_flag`; callers wanting the default value of `1` should pass `1` here.
    pub fn set_flag_value(self, flag: sys::switch_channel_flag_t, value: u32) {
        // SAFETY: `self.raw` is a live channel.
        unsafe { sys::switch_channel_set_flag_value(self.raw.as_ptr(), flag, value) };
    }

    /// Sets a channel flag (`CF_*`) on this channel and any bridged peer.
    ///
    /// Wraps `switch_channel_set_flag_recursive`.
    pub fn set_flag_recursive(self, flag: sys::switch_channel_flag_t) {
        // SAFETY: `self.raw` is a live channel.
        unsafe { sys::switch_channel_set_flag_recursive(self.raw.as_ptr(), flag) };
    }

    /// Sets a channel flag (`CF_*`) on the channel's peer/bridged channel.
    ///
    /// Wraps `switch_channel_set_flag_partner`. Returns `true` when the partner flag was set.
    pub fn set_flag_partner(self, flag: sys::switch_channel_flag_t) -> bool {
        // SAFETY: `self.raw` is a live channel.
        let set = unsafe { sys::switch_channel_set_flag_partner(self.raw.as_ptr(), flag) };
        set != 0
    }

    /// Clears a channel flag (`CF_*`).
    ///
    /// Wraps `switch_channel_clear_flag`.
    pub fn clear_flag(self, flag: sys::switch_channel_flag_t) {
        // SAFETY: `self.raw` is a live channel.
        unsafe { sys::switch_channel_clear_flag(self.raw.as_ptr(), flag) };
    }

    /// Clears a channel flag (`CF_*`) from this channel and any bridged peer.
    ///
    /// Wraps `switch_channel_clear_flag_recursive`.
    pub fn clear_flag_recursive(self, flag: sys::switch_channel_flag_t) {
        // SAFETY: `self.raw` is a live channel.
        unsafe { sys::switch_channel_clear_flag_recursive(self.raw.as_ptr(), flag) };
    }

    /// Clears a channel flag (`CF_*`) from the channel's peer/bridged channel.
    ///
    /// Wraps `switch_channel_clear_flag_partner`. Returns `true` when the partner flag was
    /// cleared.
    pub fn clear_flag_partner(self, flag: sys::switch_channel_flag_t) -> bool {
        // SAFETY: `self.raw` is a live channel.
        let cleared = unsafe { sys::switch_channel_clear_flag_partner(self.raw.as_ptr(), flag) };
        cleared != 0
    }

    /// Clears a queued state-transition flag (`CF_*`).
    ///
    /// Wraps `switch_channel_clear_state_flag`. This is the inverse of
    /// [`set_state_flag`](Self::set_state_flag).
    pub fn clear_state_flag(self, flag: sys::switch_channel_flag_t) {
        // SAFETY: `self.raw` is a live channel.
        unsafe { sys::switch_channel_clear_state_flag(self.raw.as_ptr(), flag) };
    }

    /// Returns `true` when `flag` (`CF_*`) is set on the channel's peer/bridged channel.
    ///
    /// Wraps `switch_channel_test_flag_partner`.
    pub fn test_flag_partner(self, flag: sys::switch_channel_flag_t) -> bool {
        // SAFETY: `self.raw` is a live channel.
        let set = unsafe { sys::switch_channel_test_flag_partner(self.raw.as_ptr(), flag) };
        set != 0
    }

    /// Sets the per-app flag bitset registered under `app` on this channel.
    ///
    /// Wraps `switch_channel_set_app_flag_key`. Note the C signature order is `(app, channel,
    /// flags)`. Inverse of [`clear_app_flag_key`](Self::clear_app_flag_key).
    pub fn set_app_flag(self, app: impl AsRef<str>, flags: u32) -> Result<()> {
        let app = cstring(app)?;
        // SAFETY: `self.raw` is a live channel; `app` is a valid C string.
        unsafe { sys::switch_channel_set_app_flag_key(app.as_ptr(), self.raw.as_ptr(), flags) };
        Ok(())
    }

    /// Returns `true` when the per-app flag bits `flags` (registered under `app`) are set.
    ///
    /// Wraps `switch_channel_test_app_flag_key`.
    pub fn test_app_flag(self, app: impl AsRef<str>, flags: u32) -> Result<bool> {
        let app = cstring(app)?;
        // SAFETY: `self.raw` is a live channel; `app` is a valid C string.
        let set = unsafe {
            sys::switch_channel_test_app_flag_key(app.as_ptr(), self.raw.as_ptr(), flags)
        };
        Ok(set != 0)
    }

    /// Sets the channel's private flag bits (`CPF_*`).
    ///
    /// Wraps `switch_channel_set_private_flag`.
    pub fn set_private_flag(self, flags: u32) {
        // SAFETY: `self.raw` is a live channel.
        unsafe { sys::switch_channel_set_private_flag(self.raw.as_ptr(), flags) };
    }

    /// Clears the channel's private flag bits (`CPF_*`).
    ///
    /// Wraps `switch_channel_clear_private_flag`.
    pub fn clear_private_flag(self, flags: u32) {
        // SAFETY: `self.raw` is a live channel.
        unsafe { sys::switch_channel_clear_private_flag(self.raw.as_ptr(), flags) };
    }

    /// Returns `true` when the private flag bits `flags` (`CPF_*`) are set.
    ///
    /// Wraps `switch_channel_test_private_flag`.
    pub fn test_private_flag(self, flags: u32) -> bool {
        // SAFETY: `self.raw` is a live channel.
        let set = unsafe { sys::switch_channel_test_private_flag(self.raw.as_ptr(), flags) };
        set != 0
    }

    /// A human-readable summary of the channel's set `CF_*` flags.
    ///
    /// Wraps `switch_channel_get_flag_string`. The returned string borrows channel-backed storage
    /// and is tied to `&self`.
    pub fn flag_string<'a>(self) -> Option<&'a str> {
        // SAFETY: `self.raw` is a live channel. The returned pointer is null or a channel-backed
        // C string valid while `self` is live.
        let ptr = unsafe { sys::switch_channel_get_flag_string(self.raw.as_ptr()) };
        // SAFETY: `ptr` is null or a channel-backed C string valid for the lifetime of `&self`.
        unsafe { borrowed_cstr_to_str(ptr) }
    }

    // ----- Capabilities --------------------------------------------------------

    /// Sets a channel capability (`CC_*`) with an explicit integer value.
    ///
    /// Wraps `switch_channel_set_cap_value`.
    pub fn set_cap_value(self, cap: sys::switch_channel_cap_t, value: u32) {
        // SAFETY: `self.raw` is a live channel.
        unsafe { sys::switch_channel_set_cap_value(self.raw.as_ptr(), cap, value) };
    }

    /// Clears a channel capability (`CC_*`).
    ///
    /// Wraps `switch_channel_clear_cap`.
    pub fn clear_cap(self, cap: sys::switch_channel_cap_t) {
        // SAFETY: `self.raw` is a live channel.
        unsafe { sys::switch_channel_clear_cap(self.raw.as_ptr(), cap) };
    }

    /// Returns `true` when the capability `cap` (`CC_*`) is set on the channel.
    ///
    /// Wraps `switch_channel_test_cap`.
    pub fn test_cap(self, cap: sys::switch_channel_cap_t) -> bool {
        // SAFETY: `self.raw` is a live channel.
        let set = unsafe { sys::switch_channel_test_cap(self.raw.as_ptr(), cap) };
        set != 0
    }

    /// Returns `true` when the capability `cap` (`CC_*`) is set on the peer/bridged channel.
    ///
    /// Wraps `switch_channel_test_cap_partner`.
    pub fn test_cap_partner(self, cap: sys::switch_channel_cap_t) -> bool {
        // SAFETY: `self.raw` is a live channel.
        let set = unsafe { sys::switch_channel_test_cap_partner(self.raw.as_ptr(), cap) };
        set != 0
    }

    /// A human-readable summary of the channel's set `CC_*` capabilities.
    ///
    /// Wraps `switch_channel_get_cap_string`. The returned string borrows channel-backed storage
    /// and is tied to `&self`.
    pub fn cap_string<'a>(self) -> Option<&'a str> {
        // SAFETY: `self.raw` is a live channel. The returned pointer is null or a channel-backed
        // C string valid while `self` is live.
        let ptr = unsafe { sys::switch_channel_get_cap_string(self.raw.as_ptr()) };
        // SAFETY: `ptr` is null or a channel-backed C string valid for the lifetime of `&self`.
        unsafe { borrowed_cstr_to_str(ptr) }
    }

    // ----- Private (opaque pointer) storage ------------------------------------

    /// Stores an opaque private pointer under `key` (e.g. a tech-private struct).
    ///
    /// Wraps `switch_channel_set_private`. The caller is responsible for the lifetime of whatever
    /// `info` points at; the channel merely holds the pointer.
    pub fn set_private(self, key: impl AsRef<str>, info: *mut std::ffi::c_void) -> Result<()> {
        let key = cstring(key)?;
        // SAFETY: `self.raw` is a live channel; `key` is a valid C string; `info` is caller-owned.
        let status = unsafe {
            sys::switch_channel_set_private(self.raw.as_ptr(), key.as_ptr(), info.cast_const())
        };
        status_to_result(status)
    }

    /// Retrieves the opaque private pointer previously stored under `key`.
    ///
    /// Wraps `switch_channel_get_private`. Returns `None` when nothing is stored under `key`.
    /// This is a raw-pointer escape hatch; the pointer borrows the channel's private store.
    pub fn get_private(self, key: impl AsRef<str>) -> Result<Option<*mut std::ffi::c_void>> {
        let key = cstring(key)?;
        // SAFETY: `self.raw` is a live channel; `key` is a valid C string.
        let ptr = unsafe { sys::switch_channel_get_private(self.raw.as_ptr(), key.as_ptr()) };
        if ptr.is_null() {
            Ok(None)
        } else {
            Ok(Some(ptr))
        }
    }

    /// Retrieves the opaque private pointer stored under `key` on the peer/bridged channel.
    ///
    /// Wraps `switch_channel_get_private_partner`. Returns `None` when nothing is stored.
    pub fn get_private_partner(
        self,
        key: impl AsRef<str>,
    ) -> Result<Option<*mut std::ffi::c_void>> {
        let key = cstring(key)?;
        // SAFETY: `self.raw` is a live channel; `key` is a valid C string.
        let ptr =
            unsafe { sys::switch_channel_get_private_partner(self.raw.as_ptr(), key.as_ptr()) };
        if ptr.is_null() {
            Ok(None)
        } else {
            Ok(Some(ptr))
        }
    }
}

/// Builds a temporary `switch_event` via `populate`, copies its headers into owned `(name, value)`
/// pairs, then destroys the event.
///
/// `populate` receives a `*mut *mut sys::switch_event_t` out-param it must initialize on success.
/// The event is always destroyed (even on a non-success status, when FreeSWITCH may have partially
/// populated it) so no pool memory is leaked.
fn collect_channel_variables(
    populate: impl FnOnce(*mut *mut sys::switch_event_t) -> sys::switch_status_t,
) -> Result<Vec<(String, String)>> {
    let mut event: *mut sys::switch_event_t = std::ptr::null_mut();
    // SAFETY: `event` is a null out-param; `populate` (a closure over a live channel) fills it on
    // success. We destroy it below regardless of status.
    let status = populate(&mut event);
    let result = if status == crate::SUCCESS && !event.is_null() {
        // SAFETY: `event` is non-null and a valid event populated by FreeSWITCH. We walk the
        // `headers` linked list copying each name/value into owned Rust strings.
        let mut pairs = Vec::new();
        let mut header = unsafe { (*event).headers };
        while !header.is_null() {
            // SAFETY: `header` is non-null and points at a valid event header node.
            let node = unsafe { &*header };
            if !node.name.is_null() {
                // SAFETY: `node.name`/`node.value` are null or valid C strings backed by the
                // event's pool, valid for the duration of this copy.
                let name = borrowed_cstr_to_string(node.name.cast_const());
                let value = borrowed_cstr_to_string(node.value.cast_const());
                if let Some(name) = name {
                    pairs.push((name, value.unwrap_or_default()));
                }
            }
            header = node.next;
        }
        Ok(pairs)
    } else {
        Err(crate::SwitchError(status))
    };
    if !event.is_null() {
        // SAFETY: `event` is non-null and a valid event; `switch_event_destroy` frees it.
        unsafe { sys::switch_event_destroy(&mut event) };
    }
    result
}

/// Translates a cause name (e.g. `"normal_clearing"`) into a [`Cause`].
pub fn str_to_cause(name: impl AsRef<str>) -> Result<Cause> {
    let name = cstring(name)?;
    // SAFETY: `name` is a valid C string for the call.
    Ok(unsafe { sys::switch_channel_str2cause(name.as_ptr()) })
}

/// Translates a [`Cause`] into its canonical name. The returned string borrows static storage.
pub fn cause_to_str(cause: Cause) -> Option<&'static str> {
    // SAFETY: `switch_channel_cause2str` returns a static string literal.
    let ptr = unsafe { sys::switch_channel_cause2str(cause) };
    // SAFETY: `ptr` is null or a static null-terminated string.
    unsafe { borrowed_cstr_to_str(ptr) }
}

/// Globally registers a device-state callback. `function` is invoked with the affected session,
/// call-state, and device record whenever a channel's device state changes; `user_data` is passed
/// through unchanged.
///
/// This is a global (not per-channel) registration: the callback and `user_data` must remain valid
/// for the lifetime of the FreeSWITCH process (or until removed). Returns `Err` on failure.
///
/// # Safety escape hatch
///
/// `user_data` is an opaque raw pointer whose lifetime the caller must manage; it is stored and
/// later passed back to `function` by FreeSWITCH.
pub fn bind_device_state_handler(
    function: sys::switch_device_state_function_t,
    user_data: *mut std::os::raw::c_void,
) -> Result<()> {
    // SAFETY: `function` is null or a valid C callback; `user_data` is opaque per the caller's
    // contract and lives for the registration's duration.
    let status = unsafe { sys::switch_channel_bind_device_state_handler(function, user_data) };
    status_to_result(status)
}

/// Globally removes a previously-registered device-state callback. Returns `Err` on failure.
pub fn unbind_device_state_handler(function: sys::switch_device_state_function_t) -> Result<()> {
    // SAFETY: `function` is null or a callback previously registered with
    // [`bind_device_state_handler`].
    let status = unsafe { sys::switch_channel_unbind_device_state_handler(function) };
    status_to_result(status)
}
