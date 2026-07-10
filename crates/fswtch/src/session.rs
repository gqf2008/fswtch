use std::ffi::c_char;
use std::ptr::NonNull;

use crate::{Result, cstring, status_to_result, strdup_to_string, sys};

#[derive(Copy, Clone)]
pub struct Session {
    raw: NonNull<sys::switch_core_session_t>,
}

impl Session {
    /// Wraps a FreeSWITCH session pointer for the duration of a callback.
    ///
    /// # Safety
    ///
    /// `raw` must point to a live FreeSWITCH session and remain valid while this wrapper is used.
    pub unsafe fn from_raw(raw: *mut sys::switch_core_session_t) -> Option<Self> {
        NonNull::new(raw).map(|raw| Self { raw })
    }

    #[inline]
    pub fn as_ptr(self) -> *mut sys::switch_core_session_t {
        self.raw.as_ptr()
    }

    /// Spawns the session's state-machine thread (`switch_core_session_thread_launch`).
    ///
    /// FreeSWITCH's state machine (`switch_core_session_run`) runs on this thread, driving the
    /// channel through `CS_NEW` → `CS_INIT` → … transitions. For an outgoing leg created by an
    /// endpoint's `outgoing_channel` callback, the originator (`switch_ivr_originate`) normally
    /// calls this itself after the callback returns `CAUSE_SUCCESS` — endpoint authors usually
    /// do **not** need to call it. It is exposed for endpoints that need to drive a session
    /// outside the standard originate path.
    pub fn thread_launch(self) -> Result<()> {
        // SAFETY: `self.raw` is a live, fully-initialized session. The function checks the
        // `SSF_THREAD_RUNNING`/`SSF_THREAD_STARTED` flags itself and returns `INUSE`/`FALSE`
        // on a double-launch, which `status_to_result` maps to `Err`.
        let status = unsafe { sys::switch_core_session_thread_launch(self.raw.as_ptr()) };
        status_to_result(status)
    }

    /// The read codec's actual sample rate (Hz). Returns `8000` when no codec is
    /// set (e.g. before media is negotiated) so callers get a sane default.
    pub fn read_sample_rate(self) -> u32 {
        // SAFETY: `self.raw` is a live session. `get_read_codec` returns a
        // pointer valid for the session's lifetime (or null).
        let codec = unsafe { sys::switch_core_session_get_read_codec(self.raw.as_ptr()) };
        if codec.is_null() {
            return 8000;
        }
        // SAFETY: `codec` is non-null and live; `implementation` is a pointer
        // populated by FreeSWITCH during codec init.
        let imp = unsafe { (*codec).implementation };
        if imp.is_null() {
            return 8000;
        }
        // SAFETY: `imp` is non-null and owned by the codec.
        unsafe { (*imp).actual_samples_per_second }
    }

    /// The read codec's samples-per-packet (e.g. 160 for 8 kHz / 20 ms L16).
    /// Returns `160` as a default when no codec is set.
    pub fn read_samples_per_packet(self) -> u32 {
        // SAFETY: see `read_sample_rate`.
        let codec = unsafe { sys::switch_core_session_get_read_codec(self.raw.as_ptr()) };
        if codec.is_null() {
            return 160;
        }
        let imp = unsafe { (*codec).implementation };
        if imp.is_null() {
            return 160;
        }
        // SAFETY: `imp` is non-null and owned by the codec.
        unsafe { (*imp).samples_per_packet }
    }

    /// Allocates a `switch_codec_t` on the session's pool and initializes it as
    /// the session's read codec.
    ///
    /// `implementation` is the codec module name (e.g. `"L16"`, `"PCMU"`); `rate`
    /// the sample rate in Hz; `ms` the packetization interval; `channels` the
    /// channel count. The codec struct lives on the session pool — no `Drop`
    /// needed, it is reclaimed when the session is destroyed.
    ///
    /// Endpoints that synthesize an outgoing leg (no real signalling stack) call
    /// this from `outgoing_channel` so the bridge has a codec to transcode
    /// against. Without a read codec, `switch_core_io` hangs the channel up with
    /// `SWITCH_CAUSE_INCOMPATIBLE_DESTINATION` as soon as media exchange begins.
    pub fn init_read_codec(
        self,
        implementation: impl AsRef<str>,
        rate: u32,
        ms: u32,
        channels: u32,
    ) -> Result<()> {
        let codec = self.alloc_codec_on_pool(implementation, rate, ms, channels)?;
        // SAFETY: `self.raw` is a live session; `codec` is a valid codec struct
        // allocated on this session's pool. `set_read_codec` stores the pointer
        // for the session's lifetime (pool-owned, so no dangling on drop).
        let status = unsafe { sys::switch_core_session_set_read_codec(self.raw.as_ptr(), codec) };
        status_to_result(status)
    }

    /// Same as [`init_read_codec`](Self::init_read_codec) but for the write codec.
    pub fn init_write_codec(
        self,
        implementation: impl AsRef<str>,
        rate: u32,
        ms: u32,
        channels: u32,
    ) -> Result<()> {
        let codec = self.alloc_codec_on_pool(implementation, rate, ms, channels)?;
        // SAFETY: see `init_read_codec`.
        let status = unsafe { sys::switch_core_session_set_write_codec(self.raw.as_ptr(), codec) };
        status_to_result(status)
    }

    /// Helper: allocate a zeroed `switch_codec_t` on the session pool and
    /// initialize it via `switch_core_codec_init_with_bitrate`. Returns a
    /// pool-owned raw pointer (no `Drop`).
    fn alloc_codec_on_pool(
        self,
        implementation: impl AsRef<str>,
        rate: u32,
        ms: u32,
        channels: u32,
    ) -> Result<*mut sys::switch_codec_t> {
        let implementation = cstring(implementation)?;
        // SAFETY: `self.raw` is a live session; `get_pool` returns its pool.
        let pool = unsafe { sys::switch_core_session_get_pool(self.raw.as_ptr()) };
        if pool.is_null() {
            return Err(crate::SwitchError(crate::GENERR));
        }
        // SAFETY: `switch_core_perform_session_alloc` allocates `size` bytes on
        // the session pool, zeroed/aligned suitably for any struct.
        let codec = unsafe {
            sys::switch_core_perform_session_alloc(
                self.raw.as_ptr(),
                std::mem::size_of::<sys::switch_codec_t>() as _,
                c"fswtch-rs".as_ptr(),
                c"Session::alloc_codec_on_pool".as_ptr(),
                line!() as _,
            )
        };
        if codec.is_null() {
            return Err(crate::SwitchError(crate::GENERR));
        }
        // SAFETY: `codec` is a freshly pool-allocated buffer of the right size;
        // zero it before init (matches bindgen's Default). The pool owns it.
        unsafe {
            std::ptr::write_bytes::<u8>(
                codec.cast::<u8>(),
                0,
                std::mem::size_of::<sys::switch_codec_t>(),
            )
        };
        // SAFETY: `codec` is a valid, zeroed, pool-owned `switch_codec_t`;
        // SAFETY: `codec` is a valid, zeroed, pool-owned `switch_codec_t`;
        // `implementation` is a valid C string; `pool` is this session's pool.
        //
        // flags MUST include ENCODE | DECODE: an endpoint's synthesized codec is
        // bidirectional (the bridge both writes caller audio through it and
        // reads our TTS through it). With flags=0 the codec module never runs
        // its decoder/encoder init, and the first transcode attempt fails with
        // "Codec decoder is not initialized" (switch_core_codec.c:815) →
        // INCOMPATIBLE_DESTINATION hangup. This matches how mod_loopback's
        // tech_init initializes its codecs.
        let codec_flags = sys::switch_codec_flag_enum_t_SWITCH_CODEC_FLAG_ENCODE
            | sys::switch_codec_flag_enum_t_SWITCH_CODEC_FLAG_DECODE;
        let status = unsafe {
            sys::switch_core_codec_init_with_bitrate(
                codec.cast::<sys::switch_codec_t>(),
                implementation.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                rate,
                ms as std::os::raw::c_int,
                channels as std::os::raw::c_int,
                0,
                codec_flags,
                std::ptr::null(),
                pool,
            )
        };
        status_to_result(status)?;
        Ok(codec.cast::<sys::switch_codec_t>())
    }

    pub fn answer(self) -> Result<()> {
        // SAFETY: `self.raw` is a live session pointer provided by FreeSWITCH.
        let channel = unsafe { sys::switch_core_session_get_channel(self.raw.as_ptr()) };
        let Some(channel) = NonNull::new(channel) else {
            return Ok(());
        };

        // SAFETY: `channel` belongs to `self.raw`; source strings are static C strings.
        let status = unsafe {
            sys::switch_channel_perform_answer(
                channel.as_ptr(),
                c"fswtch-rs".as_ptr(),
                c"Session::answer".as_ptr(),
                line!() as _,
            )
        };
        status_to_result(status)
    }

    pub fn sleep_ms(self, milliseconds: u32) -> Result<()> {
        // SAFETY: `self.raw` is a live session pointer provided by FreeSWITCH.
        let status = unsafe {
            sys::switch_ivr_sleep(
                self.raw.as_ptr(),
                milliseconds,
                sys::switch_bool_t_SWITCH_FALSE,
                std::ptr::null_mut(),
            )
        };
        status_to_result(status)
    }

    pub fn play_file(self, path: impl AsRef<str>) -> Result<()> {
        let path = cstring(path)?;
        // SAFETY: `self.raw` is live and `path` is a valid C string for the duration of the call.
        let status = unsafe {
            sys::switch_ivr_play_file(
                self.raw.as_ptr(),
                std::ptr::null_mut(),
                path.as_ptr(),
                std::ptr::null_mut(),
            )
        };
        status_to_result(status)
    }

    /// The channel backing this session.
    pub fn channel(self) -> Option<crate::Channel> {
        // SAFETY: `self.raw` is a live session; its channel is live for the session's lifetime.
        let raw = unsafe { sys::switch_core_session_get_channel(self.raw.as_ptr()) };
        // SAFETY: The channel borrows the session and is live while `self` is.
        unsafe { crate::Channel::from_raw(raw) }
    }

    /// The session's UUID (the channel UUID's canonical form for inbound
    /// sessions). Wraps `switch_core_session_get_uuid`. The returned string
    /// is borrowed from FreeSWITCH for the call's duration.
    pub fn uuid(self) -> Option<String> {
        // SAFETY: `self.raw` is a live session.
        let ptr = unsafe { sys::switch_core_session_get_uuid(self.raw.as_ptr()) };
        crate::command::borrowed_cstr_to_string(ptr.cast_const())
    }

    /// Hangs up the session's channel with the given cause.
    pub fn hangup(self, cause: crate::Cause) {
        // SAFETY: `self.raw` is a live session; its channel is live for the session's lifetime.
        let raw = unsafe { sys::switch_core_session_get_channel(self.raw.as_ptr()) };
        // SAFETY: The channel borrows the session and is live while `self` is.
        if let Some(channel) = unsafe { crate::Channel::from_raw(raw) } {
            channel.hangup(cause);
        }
    }

    /// Executes a dialplan application by name (e.g. `"playback"`, `"park"`) with the given argument
    /// string. Pass an empty `data` when the application takes none.
    pub fn execute_application(self, app: impl AsRef<str>, data: &str) -> Result<()> {
        let app = cstring(app)?;
        let data = cstring(data)?;
        // SAFETY: `self.raw` is a live session; both C strings are valid for the call; a null flags
        // pointer means the caller does not want the application flags back.
        let status = unsafe {
            sys::switch_core_session_execute_application_get_flags(
                self.raw.as_ptr(),
                app.as_ptr(),
                data.as_ptr(),
                std::ptr::null_mut(),
            )
        };
        status_to_result(status)
    }

    /// Sends a DTMF digit string (e.g. `"123#"`, `"*"`) on the session's media
    /// path. Each character must be one of `0-9`, `*`, `#`, or `A-D`.
    ///
    /// Wraps `switch_core_session_send_dtmf_string` (the bindgen-generated
    /// binding, not a hand-written signature) so call-control code no longer
    /// needs its own `unsafe extern "C"` shim.
    pub fn send_dtmf(self, digits: impl AsRef<str>) -> Result<()> {
        let digits = cstring(digits)?;
        // SAFETY: `self.raw` is a live session; `digits` is a valid C string for the call.
        let status = unsafe {
            sys::switch_core_session_send_dtmf_string(self.raw.as_ptr(), digits.as_ptr())
        };
        status_to_result(status)
    }
}

/// RAII guard for a session looked up by UUID via `switch_core_session_perform_locate`.
///
/// The session is read-locked for the guard's lifetime; `switch_core_session_rwunlock` runs on drop.
/// The borrowed [`Session`] returned by [`session`](Self::session) must not outlive this guard.
pub struct SessionGuard {
    inner: Option<Session>,
}

impl SessionGuard {
    /// Looks up a session by UUID and read-locks it. Returns `Ok(None)` when no such session exists.
    pub fn locate(uuid: impl AsRef<str>) -> Result<Option<Self>> {
        let uuid = cstring(uuid)?;
        // SAFETY: `uuid` is a valid C string for the call.
        Ok(unsafe { Self::from_uuid(uuid.as_ptr()) })
    }

    /// # Safety
    ///
    /// `uuid` must be a valid null-terminated C string for the duration of the call.
    // SAFETY: The caller must supply a valid C string.
    unsafe fn from_uuid(uuid: *const c_char) -> Option<Self> {
        // SAFETY: `uuid` is a valid C string per the caller's contract.
        let raw = unsafe {
            sys::switch_core_session_perform_locate(
                uuid,
                c"fswtch-rs".as_ptr(),
                c"SessionGuard::locate".as_ptr(),
                line!() as _,
            )
        };
        // SAFETY: `raw` is a live, read-locked session when non-null.
        let session = unsafe { Session::from_raw(raw) }?;
        Some(Self {
            inner: Some(session),
        })
    }

    /// The read-locked session. The borrow is tied to this guard; do not let it outlive the guard.
    pub fn session(&self) -> Option<&Session> {
        self.inner.as_ref()
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        if let Some(session) = self.inner.take() {
            // SAFETY: `session.as_ptr()` is the read-locked session obtained from `perform_locate`.
            unsafe { sys::switch_core_session_rwunlock(session.as_ptr()) };
        }
    }
}

/// Sends a dialplan application to a session by UUID via FreeSWITCH's `sendmsg`
/// (`switch_core_session_message_send` + `SWITCH_MESSAGE_INDICATE_BROADCAST`).
///
/// **Despite the name, this call is synchronous** — `switch_core_session_message_send` delivers
/// the message inline via `switch_core_session_receive_message`, which runs the app on the
/// session's thread **and blocks the caller until it finishes**. For long-running apps like
/// `"speak"` (TTS) or `"playback"`, call this from a worker thread, not a hot path.
///
/// The "async" in the name means "runs on the session's thread" (thread-safe from any caller),
/// not "non-blocking". Unlike [`Session::execute_application`] (which runs inline on the caller's
/// thread and is only correct from the session's execution context), this is safe from any
/// thread (event handlers, API callbacks, workers). `uuid` need not be located first —
/// FreeSWITCH resolves it. Interior NUL in any string is rejected.
pub fn execute_application_async(
    uuid: impl AsRef<str>,
    app: impl AsRef<str>,
    arg: &str,
) -> Result<()> {
    let uuid = cstring(uuid)?;
    let app = cstring(app)?;
    let arg = cstring(arg)?;
    let mut msg = sys::switch_core_session_message::default();
    msg.message_id = sys::switch_core_session_message_types_t_SWITCH_MESSAGE_INDICATE_BROADCAST;
    msg.from = b"fswtch-rs\0".as_ptr() as *mut _;
    msg.string_array_arg[0] = app.as_ptr();
    msg.string_array_arg[1] = arg.as_ptr();
    // SAFETY: `uuid` is a valid C string; `msg` is a fully zero-initialized message struct with
    // a valid message id and two valid C-string args. `switch_core_session_message_send` queues
    // the message on the target session's state machine (NULL uuid → SWITCH_STATUS_FALSE).
    let status = unsafe { sys::switch_core_session_message_send(uuid.as_ptr(), &mut msg) };
    status_to_result(status)
}

// ── session event/message queue + lookup (high-frequency) ─────────────────
// These wrap the session-level event/message queue + session-lookup helpers. Pointers returned
// as `*mut sys::...` are owned by the caller and must be destroyed with the matching FreeSWITCH
// free call (e.g. `switch_event_destroy`); they are passed through raw because fswtch does not
// yet RAII every FS container type.

/// Dequeues one event from `session`'s event queue. Returns an owned `*mut switch_event_t` the
/// caller must destroy (`switch_event_destroy`), or `None` if empty. `force` bypasses
/// private-event filtering.
pub fn dequeue_event(session: Session, force: bool) -> Result<Option<*mut sys::switch_event_t>> {
    let mut ev: *mut sys::switch_event_t = std::ptr::null_mut();
    let force = if force {
        sys::switch_bool_t_SWITCH_TRUE
    } else {
        sys::switch_bool_t_SWITCH_FALSE
    };
    // SAFETY: `session.as_ptr()` live; `&mut ev` valid out-param; `force` valid bool.
    let s = unsafe { sys::switch_core_session_dequeue_event(session.as_ptr(), &mut ev, force) };
    status_to_result(s)?;
    Ok(if ev.is_null() { None } else { Some(ev) })
}

/// Queues an event onto `session`'s queue (ownership of `*event` transfers to the queue).
pub fn queue_event(session: Session, event: &mut *mut sys::switch_event_t) -> Result<()> {
    // SAFETY: live session; `event` is a valid `*mut *mut` per caller.
    status_to_result(unsafe { sys::switch_core_session_queue_event(session.as_ptr(), event) })
}

/// Receives an event into `session` (ownership of `*event` transfers).
pub fn receive_event(session: Session, event: &mut *mut sys::switch_event_t) -> Result<()> {
    // SAFETY: live session; `event` valid `*mut *mut`.
    status_to_result(unsafe { sys::switch_core_session_receive_event(session.as_ptr(), event) })
}

/// Number of events queued on `session`.
pub fn event_count(session: Session) -> u32 {
    // SAFETY: live session.
    unsafe { sys::switch_core_session_event_count(session.as_ptr()) }
}

/// Sends an event to the session identified by `uuid` (ownership of `*event` transfers).
pub fn event_send(uuid: impl AsRef<str>, event: &mut *mut sys::switch_event_t) -> Result<()> {
    let uuid = cstring(uuid)?;
    // SAFETY: valid C string; `event` valid `*mut *mut`.
    status_to_result(unsafe { sys::switch_core_session_event_send(uuid.as_ptr(), event) })
}

/// Number of messages waiting on `session`.
pub fn messages_waiting(session: Session) -> u32 {
    // SAFETY: live session.
    unsafe { sys::switch_core_session_messages_waiting(session.as_ptr()) }
}

/// Dequeues one message from `session`. Returns an owned `*mut switch_core_session_message_t`
/// (caller frees via `switch_core_session_free_message`), or `None` if empty.
pub fn dequeue_message(
    session: Session,
) -> Result<Option<*mut sys::switch_core_session_message_t>> {
    let mut m: *mut sys::switch_core_session_message_t = std::ptr::null_mut();
    // SAFETY: live session; `&mut m` valid out.
    let s = unsafe { sys::switch_core_session_dequeue_message(session.as_ptr(), &mut m) };
    status_to_result(s)?;
    Ok(if m.is_null() { None } else { Some(m) })
}

/// Queues a message onto `session` (`message` borrowed for the call).
pub fn queue_message(
    session: Session,
    message: *mut sys::switch_core_session_message_t,
) -> Result<()> {
    // SAFETY: live session; `message` valid per caller.
    status_to_result(unsafe { sys::switch_core_session_queue_message(session.as_ptr(), message) })
}

/// Finds every session whose channel variable `var_name == var_val`. Returns a
/// `*mut switch_console_callback_match_t` (null if none) the caller must destroy via
/// `switch_console_callback_match_destroy`.
pub fn findall_matching_var(
    var_name: impl AsRef<str>,
    var_val: impl AsRef<str>,
) -> Result<*mut sys::switch_console_callback_match_t> {
    let n = cstring(var_name)?;
    let v = cstring(var_val)?;
    // SAFETY: both valid C strings; returns null or a match struct the caller destroys.
    Ok(unsafe { sys::switch_core_session_findall_matching_var(n.as_ptr(), v.as_ptr()) })
}

/// Finds every active session. Returns a match struct (null if none) the caller destroys.
pub fn findall() -> *mut sys::switch_console_callback_match_t {
    // SAFETY: no args.
    unsafe { sys::switch_core_session_findall() }
}

/// Looks up an application's flags by name. Returns the flags bitmask, or `Err` if the app is
/// not registered.
pub fn get_app_flags(app: impl AsRef<str>) -> Result<i32> {
    let app = cstring(app)?;
    let mut flags: i32 = 0;
    // SAFETY: valid C string; `&mut flags` valid out.
    status_to_result(unsafe { sys::switch_core_session_get_app_flags(app.as_ptr(), &mut flags) })?;
    Ok(flags)
}

/// The session's application log (a `*mut switch_app_log_t` borrowed from the session; do not free).
pub fn get_app_log(session: Session) -> *mut sys::switch_app_log_t {
    // SAFETY: live session; the returned pointer borrows session storage.
    unsafe { sys::switch_core_session_get_app_log(session.as_ptr()) }
}

/// URL-encodes `url` against the session's vars (substitutes `${var}` etc.). Returns the owned
/// string, or `None` if encoding produced nothing.
pub fn session_url_encode(session: Session, url: impl AsRef<str>) -> Option<String> {
    let url = match cstring(url) {
        Ok(s) => s,
        Err(_) => return None,
    };
    // SAFETY: live session; valid C string; returns null or a malloc'd C string.
    let ptr = unsafe { sys::switch_core_session_url_encode(session.as_ptr(), url.as_ptr()) };
    // SAFETY: `ptr` is null or a malloc'd C string; `strdup_to_string` copies it out and frees
    // the original.
    unsafe { strdup_to_string(ptr) }
}

// ── locks / state machine / media frames (high-frequency) ─────────────────

/// Read-locks `session`. Pair with `switch_core_session_rwunlock` (or prefer
/// [`SessionGuard::locate`], which read-locks and auto-unlocks). Returns `Err` if the session is
/// gone.
pub fn read_lock(session: Session) -> Result<()> {
    // SAFETY: live session.
    status_to_result(unsafe { sys::switch_core_session_read_lock(session.as_ptr()) })
}

/// Write-locks `session`. Pair with `switch_core_session_rwunlock`.
pub fn write_lock(session: Session) {
    // SAFETY: live session.
    unsafe { sys::switch_core_session_write_lock(session.as_ptr()) };
}

/// Read-lock that survives hangup (for cleanup paths). Pair with `rwunlock`.
pub fn read_lock_hangup(session: Session) -> Result<()> {
    // SAFETY: live session.
    status_to_result(unsafe { sys::switch_core_session_read_lock_hangup(session.as_ptr()) })
}

/// Soft-locks `session` for `sec` seconds (waits on hangup during that window).
pub fn soft_lock(session: Session, sec: u32) {
    // SAFETY: live session; plain u32.
    unsafe { sys::switch_core_session_soft_lock(session.as_ptr(), sec) };
}

/// Releases a soft lock.
pub fn soft_unlock(session: Session) {
    // SAFETY: live session.
    unsafe { sys::switch_core_session_soft_unlock(session.as_ptr()) };
}

/// Receives an outbound DTMF on `session` (`dtmf` borrowed for the call).
pub fn recv_dtmf(session: Session, dtmf: *const sys::switch_dtmf_t) -> Result<()> {
    // SAFETY: live session; `dtmf` valid per caller.
    status_to_result(unsafe { sys::switch_core_session_recv_dtmf(session.as_ptr(), dtmf) })
}

/// Signals a state change on `session` (re-runs the state machine).
pub fn signal_state_change(session: Session) {
    // SAFETY: live session.
    unsafe { sys::switch_core_session_signal_state_change(session.as_ptr()) };
}

/// Runs the reporting state on `session`.
pub fn reporting_state(session: Session) {
    // SAFETY: live session.
    unsafe { sys::switch_core_session_reporting_state(session.as_ptr()) };
}

/// Runs the hangup state on `session`. `force` bypasses state checks.
pub fn hangup_state(session: Session, force: bool) {
    let force = if force {
        sys::switch_bool_t_SWITCH_TRUE
    } else {
        sys::switch_bool_t_SWITCH_FALSE
    };
    // SAFETY: live session; valid bool.
    unsafe { sys::switch_core_session_hangup_state(session.as_ptr(), force) };
}

/// Resets `session`: `flush_dtmf` clears the DTMF queue, `reset_read_codec` re-inits the read
/// codec. Named `reset_session` to avoid clashing with [`crate::limit::reset`].
pub fn reset_session(session: Session, flush_dtmf: bool, reset_read_codec: bool) {
    let fd = if flush_dtmf {
        sys::switch_bool_t_SWITCH_TRUE
    } else {
        sys::switch_bool_t_SWITCH_FALSE
    };
    let rc = if reset_read_codec {
        sys::switch_bool_t_SWITCH_TRUE
    } else {
        sys::switch_bool_t_SWITCH_FALSE
    };
    // SAFETY: live session; two valid bools.
    unsafe { sys::switch_core_session_reset(session.as_ptr(), fd, rc) };
}

/// Stops media on `session` (tears down RTP/media).
pub fn stop_media(session: Session) {
    // SAFETY: live session.
    unsafe { sys::switch_core_session_stop_media(session.as_ptr()) };
}

/// `true` if `session_a` and `session_b` are transcoding `type_` media between each other.
pub fn transcoding(
    session_a: Session,
    session_b: Session,
    type_: sys::switch_media_type_t,
) -> bool {
    // SAFETY: both sessions live; `type_` valid enum.
    let r = unsafe {
        sys::switch_core_session_transcoding(session_a.as_ptr(), session_b.as_ptr(), type_)
    };
    r != sys::switch_bool_t_SWITCH_FALSE
}

/// `true` if `session`'s state-machine thread is running.
pub fn running(session: Session) -> bool {
    // SAFETY: live session.
    unsafe { sys::switch_core_session_running(session.as_ptr()) != 0 }
}

/// `true` if `session` has started (passed CS_INIT).
pub fn started(session: Session) -> bool {
    // SAFETY: live session.
    unsafe { sys::switch_core_session_started(session.as_ptr()) != 0 }
}

/// The session's numeric id.
pub fn session_id(session: Session) -> u64 {
    // SAFETY: live session.
    unsafe { sys::switch_core_session_get_id(session.as_ptr()) as u64 }
}

/// The global session id counter.
pub fn current_session_id() -> u64 {
    // SAFETY: no args.
    unsafe { sys::switch_core_session_id() as u64 }
}

/// Sets the session's external id (a caller-supplied string). Interior NUL rejected.
pub fn set_external_id(session: Session, external_id: impl AsRef<str>) -> Result<()> {
    let id = cstring(external_id)?;
    // SAFETY: live session; valid C string.
    status_to_result(unsafe {
        sys::switch_core_session_set_external_id(session.as_ptr(), id.as_ptr())
    })
}

/// The session's external id (borrowed from session storage; do not free). Null if unset.
pub fn external_id(session: Session) -> *const std::os::raw::c_char {
    // SAFETY: live session; returns null or a borrowed static-ish string.
    unsafe { sys::switch_core_session_get_external_id(session.as_ptr()) }
}

/// Reads/sets the session-count limit. Pass `0` to read without changing; nonzero sets it.
/// Returns the previous value.
pub fn session_limit(new_limit: u32) -> u32 {
    // SAFETY: plain u32.
    unsafe { sys::switch_core_session_limit(new_limit) }
}

/// Reads one media frame from `session` into `*frame` (out-param). `flags`/`stream_id` per FS.
pub fn read_frame(
    session: Session,
    frame: &mut *mut sys::switch_frame_t,
    flags: sys::switch_io_flag_t,
    stream_id: i32,
) -> Result<()> {
    // SAFETY: live session; `frame` valid out; plain args.
    status_to_result(unsafe {
        sys::switch_core_session_read_frame(session.as_ptr(), frame, flags, stream_id)
    })
}

/// Writes one media frame to `session`. `frame` borrowed; `flags`/`stream_id` per FS.
pub fn write_frame(
    session: Session,
    frame: *mut sys::switch_frame_t,
    flags: sys::switch_io_flag_t,
    stream_id: i32,
) -> Result<()> {
    // SAFETY: live session; `frame` valid; plain args.
    status_to_result(unsafe {
        sys::switch_core_session_write_frame(session.as_ptr(), frame, flags, stream_id)
    })
}

/// Sets the per-session log level.
pub fn set_loglevel(session: Session, loglevel: sys::switch_log_level_t) -> Result<()> {
    // SAFETY: live session; valid enum.
    status_to_result(unsafe { sys::switch_core_session_set_loglevel(session.as_ptr(), loglevel) })
}
