use std::{
    ffi::{CStr, c_void},
    marker::PhantomData,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr::NonNull,
    slice,
};

use crate::{
    Result, Session, StaticCStr, SwitchError, borrowed_cstr_to_str, cstring, log_error,
    status_to_result, sys,
};

macro_rules! call_ffi {
    ($call:expr) => {{
        // SAFETY: The caller documents the FreeSWITCH ABI preconditions at each call site.
        unsafe { $call }
    }};
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum MediaBugAction {
    Continue,
    Stop,
}

impl MediaBugAction {
    fn as_switch_bool(self) -> sys::switch_bool_t {
        match self {
            Self::Continue => sys::switch_bool_t_SWITCH_TRUE,
            Self::Stop => sys::switch_bool_t_SWITCH_FALSE,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct MediaBugFlags(pub sys::switch_media_bug_flag_t);

impl MediaBugFlags {
    pub const BOTH: Self = Self(sys::switch_media_bug_flag_enum_t_SMBF_BOTH);
    pub const READ_STREAM: Self = Self(sys::switch_media_bug_flag_enum_t_SMBF_READ_STREAM);
    pub const WRITE_STREAM: Self = Self(sys::switch_media_bug_flag_enum_t_SMBF_WRITE_STREAM);
    pub const WRITE_REPLACE: Self = Self(sys::switch_media_bug_flag_enum_t_SMBF_WRITE_REPLACE);
    pub const READ_REPLACE: Self = Self(sys::switch_media_bug_flag_enum_t_SMBF_READ_REPLACE);
    pub const READ_PING: Self = Self(sys::switch_media_bug_flag_enum_t_SMBF_READ_PING);
    pub const STEREO: Self = Self(sys::switch_media_bug_flag_enum_t_SMBF_STEREO);
    pub const ANSWER_REQUIRED: Self = Self(sys::switch_media_bug_flag_enum_t_SMBF_ANSWER_REQ);
    pub const BRIDGE_REQUIRED: Self = Self(sys::switch_media_bug_flag_enum_t_SMBF_BRIDGE_REQ);
    pub const THREAD_LOCK: Self = Self(sys::switch_media_bug_flag_enum_t_SMBF_THREAD_LOCK);
    pub const PRUNE: Self = Self(sys::switch_media_bug_flag_enum_t_SMBF_PRUNE);
    pub const NO_PAUSE: Self = Self(sys::switch_media_bug_flag_enum_t_SMBF_NO_PAUSE);
    pub const STEREO_SWAP: Self = Self(sys::switch_media_bug_flag_enum_t_SMBF_STEREO_SWAP);
    pub const LOCK: Self = Self(sys::switch_media_bug_flag_enum_t_SMBF_LOCK);
    pub const TAP_NATIVE_READ: Self = Self(sys::switch_media_bug_flag_enum_t_SMBF_TAP_NATIVE_READ);
    pub const TAP_NATIVE_WRITE: Self =
        Self(sys::switch_media_bug_flag_enum_t_SMBF_TAP_NATIVE_WRITE);
    pub const ONE_ONLY: Self = Self(sys::switch_media_bug_flag_enum_t_SMBF_ONE_ONLY);
    pub const READ_TEXT_STREAM: Self =
        Self(sys::switch_media_bug_flag_enum_t_SMBF_READ_TEXT_STREAM);

    pub const fn bits(self) -> sys::switch_media_bug_flag_t {
        self.0
    }
}

impl std::ops::BitOr for MediaBugFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for MediaBugFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

#[derive(Debug, Copy, Clone)]
pub struct MediaBugConfig {
    pub function: &'static CStr,
    pub target: &'static CStr,
    pub flags: MediaBugFlags,
    pub stop_time: sys::time_t,
}

impl MediaBugConfig {
    pub fn new(
        function: impl StaticCStr,
        target: impl StaticCStr,
        flags: MediaBugFlags,
    ) -> Result<Self> {
        Ok(Self {
            function: function.into_static_cstr()?,
            target: target.into_static_cstr()?,
            flags,
            stop_time: 0,
        })
    }

    pub const fn stop_time(mut self, stop_time: sys::time_t) -> Self {
        self.stop_time = stop_time;
        self
    }
}

#[derive(Debug, Copy, Clone)]
pub struct MediaBug {
    raw: NonNull<sys::switch_media_bug_t>,
}

impl MediaBug {
    pub fn as_ptr(self) -> *mut sys::switch_media_bug_t {
        self.raw.as_ptr()
    }

    /// `true` if `flag` is set on this bug.
    pub fn test_flag(self, flag: MediaBugFlags) -> bool {
        // SAFETY: `self.raw` is a live bug; `flag.bits()` is a valid bitmask.
        call_ffi!(sys::switch_core_media_bug_test_flag(self.raw.as_ptr(), flag.bits()) != 0)
    }

    /// Sets `flag` on this bug. Returns the previous state (non-zero = was set).
    pub fn set_flag(self, flag: MediaBugFlags) -> bool {
        // SAFETY: `self.raw` is a live bug; `flag.bits()` is a valid bitmask.
        call_ffi!(sys::switch_core_media_bug_set_flag(self.raw.as_ptr(), flag.bits()) != 0)
    }

    /// Clears `flag` on this bug. Returns the previous state.
    pub fn clear_flag(self, flag: MediaBugFlags) -> bool {
        // SAFETY: `self.raw` is a live bug; `flag.bits()` is a valid bitmask.
        call_ffi!(sys::switch_core_media_bug_clear_flag(self.raw.as_ptr(), flag.bits()) != 0)
    }

    /// Bytes currently buffered in the bug's read/write taps.
    pub fn inuse(self) -> (u64, u64) {
        let mut r: sys::switch_size_t = 0;
        let mut w: sys::switch_size_t = 0;
        // SAFETY: `self.raw` is a live bug; `&mut r`/`&mut w` are valid out-params.
        call_ffi!(sys::switch_core_media_bug_inuse(
            self.raw.as_ptr(),
            &mut r,
            &mut w
        ));
        (r as u64, w as u64)
    }

    /// The opaque user-data pointer attached to this bug (the handler state). Null if none;
    /// the lifetime is owned by the bug, not the caller.
    pub fn user_data(self) -> *mut c_void {
        // SAFETY: `self.raw` is a live bug.
        call_ffi!(sys::switch_core_media_bug_get_user_data(self.raw.as_ptr()))
    }

    /// A debug text dump of the bug's state, borrowed from static storage.
    pub fn text(self) -> Option<&'static str> {
        // SAFETY: `self.raw` is a live bug; returns null or a static debug string.
        let ptr = call_ffi!(sys::switch_core_media_bug_get_text(self.raw.as_ptr()));
        // SAFETY: null or a static C string.
        unsafe { borrowed_cstr_to_str(ptr) }
    }

    /// The video ping frame pointer (escape hatch; borrows bug storage).
    pub fn video_ping_frame(self) -> *mut sys::switch_frame_t {
        // SAFETY: `self.raw` is a live bug.
        call_ffi!(sys::switch_core_media_bug_get_video_ping_frame(
            self.raw.as_ptr()
        ))
    }

    /// Sets the pre-buffer frame count. Returns the previous status.
    pub fn set_pre_buffer_framecount(self, framecount: u32) -> Result<()> {
        // SAFETY: `self.raw` is a live bug; `framecount` is a plain u32.
        status_to_result(call_ffi!(
            sys::switch_core_media_bug_set_pre_buffer_framecount(self.raw.as_ptr(), framecount)
        ))
    }

    /// Copies the bug's media params into `mm` (a `switch_mm_t*` you provide). Internal/advanced.
    pub fn media_params(self, mm: *mut sys::switch_mm_t) {
        // SAFETY: `self.raw` is a live bug; `mm` is a valid `switch_mm_t*` per caller.
        call_ffi!(sys::switch_core_media_bug_get_media_params(
            self.raw.as_ptr(),
            mm
        ));
    }

    /// Applies `mm` (a `switch_mm_t*`) to the bug. Internal/advanced.
    pub fn set_media_params(self, mm: *mut sys::switch_mm_t) {
        // SAFETY: `self.raw` is a live bug; `mm` is a valid `switch_mm_t*` per caller.
        call_ffi!(sys::switch_core_media_bug_set_media_params(
            self.raw.as_ptr(),
            mm
        ));
    }

    /// Pushes a spy frame into the bug (video spy). `rw` selects read/write.
    pub fn push_spy_frame(
        self,
        frame: *mut sys::switch_frame_t,
        rw: sys::switch_rw_t,
    ) -> Result<()> {
        // SAFETY: `self.raw` is a live bug; `frame` is a valid frame pointer for the call.
        status_to_result(call_ffi!(sys::switch_core_media_bug_push_spy_frame(
            self.raw.as_ptr(),
            frame,
            rw
        )))
    }

    /// Patches a spy image into the bug (video spy). `rw` selects read/write.
    pub fn patch_spy_frame(
        self,
        img: *mut sys::switch_image_t,
        rw: sys::switch_rw_t,
    ) -> Result<()> {
        // SAFETY: `self.raw` is a live bug; `img` is a valid image pointer for the call.
        status_to_result(call_ffi!(sys::switch_core_media_bug_patch_spy_frame(
            self.raw.as_ptr(),
            img,
            rw
        )))
    }

    /// Destroys the bug. After this **all** `MediaBug` copies referencing it are invalid — do not
    /// use them. `destroy` = fully free (true) vs detach only (false). Normally FreeSWITCH
    /// manages bug lifetime via the close callback; this is an escape hatch.
    pub fn close(self, destroy: bool) -> Result<()> {
        let mut ptr = self.raw.as_ptr();
        let destroy = if destroy {
            sys::switch_bool_t_SWITCH_TRUE
        } else {
            sys::switch_bool_t_SWITCH_FALSE
        };
        // SAFETY: `ptr` is a live bug handle; `&mut ptr` is a valid `*mut *mut`; on success the
        // bug is freed and `ptr` is NULL'd.
        status_to_result(call_ffi!(sys::switch_core_media_bug_close(
            &mut ptr, destroy
        )))
    }
}

pub trait MediaBugHandler: 'static {
    fn on_init(&mut self, _ctx: &mut MediaBugContext<'_>) -> MediaBugAction {
        MediaBugAction::Continue
    }

    fn on_read(
        &mut self,
        _ctx: &mut MediaBugContext<'_>,
        _frame: MediaFrame<'_>,
    ) -> MediaBugAction {
        MediaBugAction::Continue
    }

    fn on_write(
        &mut self,
        _ctx: &mut MediaBugContext<'_>,
        _frame: MediaFrame<'_>,
    ) -> MediaBugAction {
        MediaBugAction::Continue
    }

    fn on_read_replace(
        &mut self,
        _ctx: &mut MediaBugContext<'_>,
        _frame: MediaFrameMut<'_>,
    ) -> MediaBugAction {
        MediaBugAction::Continue
    }

    fn on_write_replace(
        &mut self,
        _ctx: &mut MediaBugContext<'_>,
        _frame: MediaFrameMut<'_>,
    ) -> MediaBugAction {
        MediaBugAction::Continue
    }

    fn on_close(&mut self, _ctx: &mut MediaBugContext<'_>) {}
}

/// Attaches a FreeSWITCH media bug and owns `handler` until FreeSWITCH closes the bug.
///
pub fn attach_media_bug<H>(session: Session, config: MediaBugConfig, handler: H) -> Result<MediaBug>
where
    H: MediaBugHandler,
{
    let state = Box::into_raw(Box::new(MediaBugState { handler }));
    let mut bug = std::ptr::null_mut();

    // SAFETY: `session` is live and `state` remains allocated until close or failure cleanup.
    let status = call_ffi!(add_media_bug::<H>(session, config, state.cast(), &mut bug));

    if status != crate::SUCCESS.raw() {
        // SAFETY: FreeSWITCH did not take ownership on failure.
        call_ffi!(drop(Box::from_raw(state)));
        return Err(SwitchError(crate::Status::from_raw(status)));
    }

    let Some(raw) = NonNull::new(bug) else {
        // SAFETY: FreeSWITCH did not return a bug handle, so there is no close callback that can
        // reclaim the state.
        call_ffi!(drop(Box::from_raw(state)));
        return Err(SwitchError(crate::GENERR));
    };
    Ok(MediaBug { raw })
}

/// # Safety
///
/// `session` must be live, `user_data` must remain valid until FreeSWITCH closes the bug, and
/// `bug` must be writable output storage.
// SAFETY: The caller must provide a live session, owned user data, and writable bug output storage.
unsafe fn add_media_bug<H>(
    session: Session,
    config: MediaBugConfig,
    user_data: *mut c_void,
    bug: &mut *mut sys::switch_media_bug_t,
) -> sys::switch_status_t
where
    H: MediaBugHandler,
{
    let add = sys::switch_core_media_bug_add;
    call_ffi!(add(
        session.as_ptr(),
        config.function.as_ptr(),
        config.target.as_ptr(),
        Some(media_bug_trampoline::<H>),
        user_data,
        config.stop_time,
        config.flags.bits(),
        bug,
    ))
}

pub struct MediaBugContext<'a> {
    raw: NonNull<sys::switch_media_bug_t>,
    _lifetime: PhantomData<&'a mut sys::switch_media_bug_t>,
}

impl<'a> MediaBugContext<'a> {
    /// Wraps a media bug pointer for the duration of a FreeSWITCH media bug callback.
    ///
    /// # Safety
    ///
    /// `raw` must point to a live media bug and must remain valid for `'a`.
    pub unsafe fn from_raw(raw: *mut sys::switch_media_bug_t) -> Option<Self> {
        NonNull::new(raw).map(|raw| Self {
            raw,
            _lifetime: PhantomData,
        })
    }

    pub fn as_ptr(&self) -> *mut sys::switch_media_bug_t {
        self.raw.as_ptr()
    }

    /// The session this media bug is attached to.
    ///
    /// FreeSWITCH calls media bug callbacks under the session's read lock, so the
    /// returned session is guaranteed live for the duration of the callback.
    /// Returns a `Session` (a lightweight, `Copy` handle — does not manage the
    /// refcount; the bug owns the session for the callback's duration). Downstream
    /// code can call `session.read_sample_rate()`, `session.channel()`, etc.
    /// directly, with **no `unsafe` at the call site**.
    ///
    /// Returns `None` only if the bug has been detached (should not happen inside
    /// a callback, but is handled defensively).
    pub fn session(&self) -> Option<Session> {
        // SAFETY: `self.raw` is live for this callback; the session it returns is
        // live for the callback duration (FS holds the session read lock while
        // invoking media bug callbacks). `Session` is a non-owning handle, so
        // constructing it from a borrowed pointer is sound.
        let ptr = call_ffi!(sys::switch_core_media_bug_get_session(self.raw.as_ptr()));
        unsafe { Session::from_raw(ptr) }
    }

    pub fn native_read_frame(&self) -> Option<MediaFrame<'_>> {
        // SAFETY: `self.raw` is live for this callback.
        call_ffi!(MediaFrame::from_raw(
            sys::switch_core_media_bug_get_native_read_frame(self.raw.as_ptr())
        ))
    }

    pub fn native_write_frame(&self) -> Option<MediaFrame<'_>> {
        // SAFETY: `self.raw` is live for this callback.
        call_ffi!(MediaFrame::from_raw(
            sys::switch_core_media_bug_get_native_write_frame(self.raw.as_ptr())
        ))
    }

    pub fn read_replace_frame(&mut self) -> Option<MediaFrameMut<'_>> {
        // SAFETY: `self.raw` is live for this callback.
        call_ffi!(MediaFrameMut::from_raw(
            sys::switch_core_media_bug_get_read_replace_frame(self.raw.as_ptr())
        ))
    }

    pub fn write_replace_frame(&mut self) -> Option<MediaFrameMut<'_>> {
        // SAFETY: `self.raw` is live for this callback.
        call_ffi!(MediaFrameMut::from_raw(
            sys::switch_core_media_bug_get_write_replace_frame(self.raw.as_ptr())
        ))
    }

    pub fn set_read_replace_frame(&mut self, frame: &mut MediaFrameMut<'_>) {
        // SAFETY: Both pointers are live for this callback.
        call_ffi!(sys::switch_core_media_bug_set_read_replace_frame(
            self.raw.as_ptr(),
            frame.as_ptr()
        ));
    }

    pub fn set_write_replace_frame(&mut self, frame: &mut MediaFrameMut<'_>) {
        // SAFETY: Both pointers are live for this callback.
        call_ffi!(sys::switch_core_media_bug_set_write_replace_frame(
            self.raw.as_ptr(),
            frame.as_ptr()
        ));
    }

    pub fn flush(&mut self) {
        // SAFETY: `self.raw` is live for this callback.
        call_ffi!(sys::switch_core_media_bug_flush(self.raw.as_ptr()));
    }

    pub fn read_into(&mut self, frame: &mut sys::switch_frame_t, fill: bool) -> Result<()> {
        let fill = if fill {
            sys::switch_bool_t_SWITCH_TRUE
        } else {
            sys::switch_bool_t_SWITCH_FALSE
        };
        // SAFETY: `self.raw` is live and `frame` is caller-provided writable frame storage.
        let status = call_ffi!(sys::switch_core_media_bug_read(
            self.raw.as_ptr(),
            frame,
            fill
        ));
        status_to_result(status)
    }
}

#[derive(Copy, Clone)]
pub struct MediaFrame<'a> {
    raw: NonNull<sys::switch_frame_t>,
    _lifetime: PhantomData<&'a sys::switch_frame_t>,
}

impl<'a> MediaFrame<'a> {
    /// Wraps a frame pointer for the duration of a FreeSWITCH callback.
    ///
    /// # Safety
    ///
    /// `raw` must point to a live FreeSWITCH frame and must remain valid for `'a`.
    pub unsafe fn from_raw(raw: *mut sys::switch_frame_t) -> Option<Self> {
        NonNull::new(raw).map(|raw| Self {
            raw,
            _lifetime: PhantomData,
        })
    }

    pub fn as_ptr(self) -> *mut sys::switch_frame_t {
        self.raw.as_ptr()
    }

    pub fn data_len(self) -> usize {
        // SAFETY: `self.raw` is live for this frame wrapper.
        call_ffi!(self.raw.as_ref().datalen as usize)
    }

    pub fn samples(self) -> u32 {
        // SAFETY: `self.raw` is live for this frame wrapper.
        call_ffi!(self.raw.as_ref().samples)
    }

    pub fn rate(self) -> u32 {
        // SAFETY: `self.raw` is live for this frame wrapper.
        call_ffi!(self.raw.as_ref().rate)
    }

    pub fn channels(self) -> u32 {
        // SAFETY: `self.raw` is live for this frame wrapper.
        call_ffi!(self.raw.as_ref().channels)
    }

    pub fn bytes(self) -> &'a [u8] {
        // SAFETY: `self.raw` is live and FreeSWITCH keeps `data` valid for `datalen` bytes during
        // the callback. Null data with zero length is represented as an empty slice.
        call_ffi!({
            let frame = self.raw.as_ref();
            if frame.data.is_null() || frame.datalen == 0 {
                &[]
            } else {
                slice::from_raw_parts(frame.data.cast::<u8>(), frame.datalen as usize)
            }
        })
    }

    pub fn pcm_i16(self) -> Option<&'a [i16]> {
        let bytes = self.bytes();
        if !bytes.len().is_multiple_of(std::mem::size_of::<i16>())
            || !(bytes.as_ptr() as usize).is_multiple_of(std::mem::align_of::<i16>())
        {
            return None;
        }

        // SAFETY: Length and alignment were checked above.
        Some(call_ffi!(slice::from_raw_parts(
            bytes.as_ptr().cast::<i16>(),
            bytes.len() / size_of::<i16>()
        )))
    }
}

pub struct MediaFrameMut<'a> {
    raw: NonNull<sys::switch_frame_t>,
    _lifetime: PhantomData<&'a mut sys::switch_frame_t>,
}

impl<'a> MediaFrameMut<'a> {
    /// Wraps a mutable frame pointer for the duration of a FreeSWITCH callback.
    ///
    /// # Safety
    ///
    /// `raw` must point to a live FreeSWITCH frame that is safe to mutate for `'a`.
    pub unsafe fn from_raw(raw: *mut sys::switch_frame_t) -> Option<Self> {
        NonNull::new(raw).map(|raw| Self {
            raw,
            _lifetime: PhantomData,
        })
    }

    pub fn as_ptr(&mut self) -> *mut sys::switch_frame_t {
        self.raw.as_ptr()
    }

    pub fn as_frame(&self) -> MediaFrame<'_> {
        MediaFrame {
            raw: self.raw,
            _lifetime: PhantomData,
        }
    }

    pub fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: `self.raw` is live and uniquely borrowed through this mutable frame wrapper.
        call_ffi!({
            let frame = self.raw.as_ref();
            if frame.data.is_null() || frame.datalen == 0 {
                &mut []
            } else {
                slice::from_raw_parts_mut(frame.data.cast::<u8>(), frame.datalen as usize)
            }
        })
    }

    pub fn pcm_i16_mut(&mut self) -> Option<&mut [i16]> {
        let bytes = self.bytes_mut();
        if !bytes.len().is_multiple_of(std::mem::size_of::<i16>())
            || !(bytes.as_ptr() as usize).is_multiple_of(std::mem::align_of::<i16>())
        {
            return None;
        }

        // SAFETY: Length and alignment were checked above, and `bytes` is uniquely borrowed.
        Some(call_ffi!(slice::from_raw_parts_mut(
            bytes.as_mut_ptr().cast::<i16>(),
            bytes.len() / size_of::<i16>(),
        )))
    }

    // ── Frame field accessors for endpoint read_frame implementations ──────
    //
    // FreeSWITCH's `switch_frame_t` distinguishes `buflen` (the capacity of the
    // data buffer in bytes) from `datalen` (the actual payload length). When
    // FreeSWITCH hands a fresh frame to an endpoint's `read_frame` callback,
    // `datalen` may be 0 even though `buflen` holds the real capacity; a
    // `read_frame` implementation must write PCM into `data` and then set
    // `datalen`/`samples` to the amounts it produced. These accessors let
    // safe code do that without touching `unsafe`.

    /// The frame's `samples` field — the number of PCM samples the codec
    /// expects in this frame (e.g. 160 for 8 kHz / 20 ms).
    pub fn samples_field(&self) -> u32 {
        // SAFETY: `self.raw` is a live frame.
        call_ffi!(self.raw.as_ref().samples)
    }

    /// The frame's `buflen` field — the capacity of `data` in bytes.
    pub fn buflen_field(&self) -> u32 {
        // SAFETY: `self.raw` is a live frame.
        call_ffi!(self.raw.as_ref().buflen)
    }

    /// The frame's `datalen` field — the current payload length in bytes.
    pub fn datalen_field(&self) -> u32 {
        // SAFETY: `self.raw` is a live frame.
        call_ffi!(self.raw.as_ref().datalen)
    }

    /// Sets the frame's `datalen` (actual payload bytes). Use after writing
    /// PCM so FreeSWITCH knows how much data the frame carries.
    pub fn set_datalen(&mut self, bytes: u32) {
        // SAFETY: `self.raw` is a live frame we may mutate.
        call_ffi!(self.raw.as_mut().datalen = bytes);
    }

    /// Sets the frame's `samples` (actual PCM sample count). Use after writing
    /// PCM so FreeSWITCH's codec layer agrees with the payload length.
    pub fn set_samples(&mut self, samples: u32) {
        // SAFETY: `self.raw` is a live frame we may mutate.
        call_ffi!(self.raw.as_mut().samples = samples);
    }

    /// Returns a mutable i16 PCM slice sized for one codec frame of output,
    /// using `samples` and `buflen` to derive the capacity (not `datalen`,
    /// which may be 0 on a fresh frame). Sets `datalen` to the byte length of
    /// the returned slice so `read_frame` callers only need to fill it.
    ///
    /// Returns `None` when `data` is null, `samples` is 0, or the buffer is
    /// too small / mis-aligned for i16.
    pub fn pcm_i16_output(&mut self) -> Option<&mut [i16]> {
        // SAFETY: `self.raw` is a live frame.
        let frame = call_ffi!(self.raw.as_ref());
        if frame.data.is_null() {
            return None;
        }
        let samples = frame.samples;
        let buflen = frame.buflen;
        if samples == 0 || buflen == 0 {
            return None;
        }
        // i16 count: no more than the codec's expected samples, and no more
        // than the buffer can hold.
        let i16_cap = (samples as usize).min((buflen as usize) / std::mem::size_of::<i16>());
        if i16_cap == 0 {
            return None;
        }
        let data_ptr = frame.data.cast::<i16>();
        if !(data_ptr as usize).is_multiple_of(std::mem::align_of::<i16>()) {
            return None;
        }
        // Set datalen so downstream sees the payload length.
        call_ffi!(self.raw.as_mut().datalen = (i16_cap * std::mem::size_of::<i16>()) as u32);
        // SAFETY: `data_ptr` is a live, non-null, i16-aligned buffer of at
        // least `i16_cap` elements (derived from `buflen`); uniquely borrowed
        // through this mutable frame wrapper.
        Some(call_ffi!(slice::from_raw_parts_mut(data_ptr, i16_cap)))
    }
}

struct MediaBugState<H> {
    handler: H,
}

/// # Safety
///
/// FreeSWITCH must call this with the `bug` and `user_data` pair supplied when the media bug was
/// registered. `user_data` must be the boxed `MediaBugState<H>` allocated by `attach_media_bug`;
/// FreeSWITCH must invoke CLOSE at most once for that pointer.
// SAFETY: FreeSWITCH must pass the same bug/user_data pair registered by `attach_media_bug`.
unsafe extern "C" fn media_bug_trampoline<H>(
    bug: *mut sys::switch_media_bug_t,
    user_data: *mut c_void,
    callback_type: sys::switch_abc_type_t,
) -> sys::switch_bool_t
where
    H: MediaBugHandler,
{
    if user_data.is_null() {
        return sys::switch_bool_t_SWITCH_TRUE;
    }

    // SAFETY: FreeSWITCH passes a live media bug pointer for the callback duration.
    let Some(mut ctx) = (call_ffi!(MediaBugContext::from_raw(bug))) else {
        return sys::switch_bool_t_SWITCH_TRUE;
    };

    if callback_type == sys::switch_abc_type_t_SWITCH_ABC_TYPE_CLOSE {
        return close_media_bug::<H>(user_data, &mut ctx);
    }

    // SAFETY: `user_data` is the `MediaBugState<H>` pointer passed to `switch_core_media_bug_add`.
    let state = call_ffi!(&mut *user_data.cast::<MediaBugState<H>>());
    let dispatch = MediaBugDispatch {
        bug,
        state,
        ctx: &mut ctx,
    };
    let result = catch_unwind(AssertUnwindSafe(|| dispatch.run(callback_type)));

    callback_result(result)
}

fn close_media_bug<H>(user_data: *mut c_void, ctx: &mut MediaBugContext<'_>) -> sys::switch_bool_t
where
    H: MediaBugHandler,
{
    // SAFETY: Close is the terminal callback for the pointer passed to FreeSWITCH.
    let mut state = call_ffi!(Box::from_raw(user_data.cast::<MediaBugState<H>>()));
    let result = catch_unwind(AssertUnwindSafe(|| {
        state.handler.on_close(ctx);
        MediaBugAction::Continue
    }));
    callback_result(result)
}

struct MediaBugDispatch<'a, H> {
    bug: *mut sys::switch_media_bug_t,
    state: &'a mut MediaBugState<H>,
    ctx: &'a mut MediaBugContext<'a>,
}

impl<H> MediaBugDispatch<'_, H>
where
    H: MediaBugHandler,
{
    fn run(self, callback_type: sys::switch_abc_type_t) -> MediaBugAction {
        match callback_type {
            sys::switch_abc_type_t_SWITCH_ABC_TYPE_INIT => self.state.handler.on_init(self.ctx),
            sys::switch_abc_type_t_SWITCH_ABC_TYPE_READ => self.read(),
            sys::switch_abc_type_t_SWITCH_ABC_TYPE_WRITE => self.write(),
            sys::switch_abc_type_t_SWITCH_ABC_TYPE_READ_REPLACE => self.read_replace(),
            sys::switch_abc_type_t_SWITCH_ABC_TYPE_WRITE_REPLACE => self.write_replace(),
            _ => MediaBugAction::Continue,
        }
    }

    fn read(self) -> MediaBugAction {
        // SAFETY: `bug` is live for the callback duration.
        let frame = call_ffi!(MediaFrame::from_raw(
            sys::switch_core_media_bug_get_native_read_frame(self.bug)
        ));
        frame.map_or(MediaBugAction::Continue, |frame| {
            self.state.handler.on_read(self.ctx, frame)
        })
    }

    fn write(self) -> MediaBugAction {
        // SAFETY: `bug` is live for the callback duration.
        let frame = call_ffi!(MediaFrame::from_raw(
            sys::switch_core_media_bug_get_native_write_frame(self.bug)
        ));
        frame.map_or(MediaBugAction::Continue, |frame| {
            self.state.handler.on_write(self.ctx, frame)
        })
    }

    fn read_replace(self) -> MediaBugAction {
        // SAFETY: `bug` is live for the callback duration.
        let frame = call_ffi!(MediaFrameMut::from_raw(
            sys::switch_core_media_bug_get_read_replace_frame(self.bug)
        ));
        frame.map_or(MediaBugAction::Continue, |frame| {
            self.state.handler.on_read_replace(self.ctx, frame)
        })
    }

    fn write_replace(self) -> MediaBugAction {
        // SAFETY: `bug` is live for the callback duration.
        let frame = call_ffi!(MediaFrameMut::from_raw(
            sys::switch_core_media_bug_get_write_replace_frame(self.bug)
        ));
        frame.map_or(MediaBugAction::Continue, |frame| {
            self.state.handler.on_write_replace(self.ctx, frame)
        })
    }
}

fn callback_result(result: std::thread::Result<MediaBugAction>) -> sys::switch_bool_t {
    match result {
        Ok(action) => action.as_switch_bool(),
        Err(_) => {
            log_error("media_bug", "media bug callback panicked");
            sys::switch_bool_t_SWITCH_FALSE
        }
    }
}

// ── session-level media-bug control ────────────────────────────────────────
// These operate on every bug attached to a session (the `*_media_bug_pause` family takes a
// session, not a single bug). Thread-safe per FreeSWITCH's media-bug locking.

/// Pauses every media bug attached to `session`.
pub fn pause_media_bugs(session: Session) {
    // SAFETY: `session.as_ptr()` is a live session.
    call_ffi!(sys::switch_core_media_bug_pause(session.as_ptr()));
}

/// Resumes every paused media bug attached to `session`.
pub fn resume_media_bugs(session: Session) {
    // SAFETY: `session.as_ptr()` is a live session.
    call_ffi!(sys::switch_core_media_bug_resume(session.as_ptr()));
}

/// Removes bugs flagged `SMBF_PRUNE` from `session`. Returns the number pruned.
pub fn prune_media_bugs(session: Session) -> u32 {
    // SAFETY: `session.as_ptr()` is a live session.
    call_ffi!(sys::switch_core_media_bug_prune(session.as_ptr()))
}

/// Flushes every media bug's buffers on `session`.
pub fn flush_all_media_bugs(session: Session) -> Result<()> {
    // SAFETY: `session.as_ptr()` is a live session.
    status_to_result(call_ffi!(sys::switch_core_media_bug_flush_all(
        session.as_ptr()
    )))
}

/// Number of media bugs on `session` registered under `function` (the name passed to
/// [`attach_media_bug`]). Interior NUL in `function` is rejected.
pub fn count_media_bugs(session: Session, function: impl AsRef<str>) -> Result<u32> {
    let function = cstring(function)?;
    // SAFETY: `session.as_ptr()` is a live session; `function` is a valid C string.
    Ok(call_ffi!(sys::switch_core_media_bug_count(
        session.as_ptr(),
        function.as_ptr()
    )))
}

/// Removes every bug on `session` registered under `function`.
pub fn remove_media_bugs_by_function(session: Session, function: impl AsRef<str>) -> Result<()> {
    let function = cstring(function)?;
    // SAFETY: live session; valid C string.
    status_to_result(call_ffi!(sys::switch_core_media_bug_remove_all_function(
        session.as_ptr(),
        function.as_ptr()
    )))
}

/// Pops (detaches) one bug registered under `function` from `session`, returning its handle.
pub fn pop_media_bug(session: Session, function: impl AsRef<str>) -> Result<Option<MediaBug>> {
    let function = cstring(function)?;
    let mut bug: *mut sys::switch_media_bug_t = std::ptr::null_mut();
    // SAFETY: live session; valid C string; `&mut bug` is a valid out-param.
    let status = call_ffi!(sys::switch_core_media_bug_pop(
        session.as_ptr(),
        function.as_ptr(),
        &mut bug
    ));
    status_to_result(status)?;
    Ok(NonNull::new(bug).map(|raw| MediaBug { raw }))
}

/// Removes `bug` from its session. The `MediaBug` handle (and all copies) is invalid afterwards.
pub fn remove_media_bug(session: Session, bug: &mut MediaBug) -> Result<()> {
    let mut ptr = bug.raw.as_ptr();
    // SAFETY: live session; `bug.raw` is a live bug; `&mut ptr` is a valid `*mut *mut`.
    let status = call_ffi!(sys::switch_core_media_bug_remove(
        session.as_ptr(),
        &mut ptr
    ));
    status_to_result(status)
}

/// Runs `cb` against every bug on `session` registered under `function`. Advanced.
pub fn exec_all_media_bugs(
    session: Session,
    function: impl AsRef<str>,
    cb: sys::switch_media_bug_exec_cb_t,
    user_data: *mut c_void,
) -> Result<()> {
    let function = cstring(function)?;
    // SAFETY: live session; valid C string; `cb` is a valid fn pointer.
    status_to_result(call_ffi!(sys::switch_core_media_bug_exec_all(
        session.as_ptr(),
        function.as_ptr(),
        cb,
        user_data
    )))
}

/// Removes every bug on `session` whose callback matches `callback`.
pub fn remove_media_bugs_by_callback(
    session: Session,
    callback: sys::switch_media_bug_callback_t,
) -> Result<()> {
    // SAFETY: live session; valid fn pointer.
    status_to_result(call_ffi!(sys::switch_core_media_bug_remove_callback(
        session.as_ptr(),
        callback
    )))
}

/// Transfers bugs matching `callback` from `orig` to `new_session`, optionally duplicating
/// `user_data` via `dup`. Advanced.
pub fn transfer_media_bug_callback(
    orig: Session,
    new_session: Session,
    callback: sys::switch_media_bug_callback_t,
    dup: Option<unsafe extern "C" fn(*mut sys::switch_core_session_t, *mut c_void) -> *mut c_void>,
) -> Result<()> {
    // SAFETY: both sessions live; valid fn pointer; `dup` is null or a matching callback.
    status_to_result(call_ffi!(sys::switch_core_media_bug_transfer_callback(
        orig.as_ptr(),
        new_session.as_ptr(),
        callback,
        dup
    )))
}

/// Enumerates the bugs on `session` into a stream handle (debug dump). `stream` is a
/// `switch_stream_handle_t*` (e.g. from an api callback's stream).
pub fn enumerate_media_bugs(
    session: Session,
    stream: *mut sys::switch_stream_handle_t,
) -> Result<()> {
    // SAFETY: live session; `stream` is a valid stream handle per caller.
    status_to_result(call_ffi!(sys::switch_core_media_bug_enumerate(
        session.as_ptr(),
        stream
    )))
}

/// Patches video bugs on `session` against `frame` (video pipeline). Returns a switch_bool_t.
pub fn patch_video_media_bugs(session: Session, frame: *mut sys::switch_frame_t) -> u32 {
    // SAFETY: live session; `frame` is a valid frame pointer for the call.
    call_ffi!(sys::switch_core_media_bug_patch_video(
        session.as_ptr(),
        frame
    ))
}

/// Sets the spy format on `bug` (video spy). Advanced.
pub fn set_spy_fmt(bug: MediaBug, spy_fmt: sys::switch_vid_spy_fmt_t) {
    // SAFETY: `bug.as_ptr()` is a live bug.
    call_ffi!(sys::switch_media_bug_set_spy_fmt(bug.as_ptr(), spy_fmt));
}

/// Parses a spy-format name (e.g. `"split"`) into a `switch_vid_spy_fmt_t`.
pub fn parse_spy_fmt(name: impl AsRef<str>) -> Result<sys::switch_vid_spy_fmt_t> {
    let name = cstring(name)?;
    // SAFETY: `name` is a valid C string.
    Ok(call_ffi!(sys::switch_media_bug_parse_spy_fmt(
        name.as_ptr()
    )))
}

// ── raw frame alloc/free/dup ───────────────────────────────────────────────

pub fn frame_alloc(frame: &mut *mut sys::switch_frame_t, size: u64) -> Result<()> {
    let mut s: sys::switch_size_t = size as _;
    // SAFETY: `&mut frame` valid out; plain size.
    status_to_result(unsafe { sys::switch_frame_alloc(frame, &mut s) })
}

pub fn frame_free(frame: &mut *mut sys::switch_frame_t) -> Result<()> {
    // SAFETY: `&mut frame` valid; frees + NULLs.
    status_to_result(unsafe { sys::switch_frame_free(frame) })
}

pub fn frame_dup(orig: *mut sys::switch_frame_t, clone: &mut *mut sys::switch_frame_t) -> Result<()> {
    // SAFETY: `orig` live; `&mut clone` valid out.
    status_to_result(unsafe { sys::switch_frame_dup(orig, clone) })
}
