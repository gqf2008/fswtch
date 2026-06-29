use std::ffi::c_char;
use std::ptr::NonNull;

use crate::{Result, cstring, status_to_result, sys};

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
        let status =
            unsafe { sys::switch_core_session_set_read_codec(self.raw.as_ptr(), codec) };
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
        let status =
            unsafe { sys::switch_core_session_set_write_codec(self.raw.as_ptr(), codec) };
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
        unsafe { std::ptr::write_bytes::<u8>(codec.cast::<u8>(), 0, std::mem::size_of::<sys::switch_codec_t>()) };
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
        let codec_flags =
            sys::switch_codec_flag_enum_t_SWITCH_CODEC_FLAG_ENCODE
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
