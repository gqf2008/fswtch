use std::{
    ffi::{CStr, c_void},
    marker::PhantomData,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr::NonNull,
    slice,
};

use crate::{Result, StaticCStr, SwitchError, log_error, status_to_result, sys};

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
/// # Safety
///
/// `session` must be a valid live FreeSWITCH session that may accept a media bug. The `function`
/// and `target` strings in `config` must remain valid for the loaded module lifetime. Callback
/// methods must not retain frame or context references after they return.
pub fn attach_media_bug<H>(
    session: *mut sys::switch_core_session_t,
    config: MediaBugConfig,
    handler: H,
) -> Result<MediaBug>
where
    H: MediaBugHandler,
{
    let state = Box::into_raw(Box::new(MediaBugState { handler }));
    let mut bug = std::ptr::null_mut();

    // SAFETY: The caller guarantees `session` is live; `state` is reclaimed on close when
    // FreeSWITCH accepts the bug, or immediately below if registration fails.
    let status = unsafe {
        sys::switch_core_media_bug_add(
            session,
            config.function.as_ptr(),
            config.target.as_ptr(),
            Some(media_bug_trampoline::<H>),
            state.cast(),
            config.stop_time,
            config.flags.bits(),
            &mut bug,
        )
    };

    if status != crate::SUCCESS {
        // SAFETY: FreeSWITCH did not take ownership on failure.
        unsafe {
            drop(Box::from_raw(state));
        }
        return Err(SwitchError(status));
    }

    let raw = NonNull::new(bug).ok_or(SwitchError(crate::GENERR))?;
    Ok(MediaBug { raw })
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

    pub fn session(&self) -> Option<NonNull<sys::switch_core_session_t>> {
        // SAFETY: `self.raw` is live for this callback.
        NonNull::new(unsafe { sys::switch_core_media_bug_get_session(self.raw.as_ptr()) })
    }

    pub fn native_read_frame(&self) -> Option<MediaFrame<'_>> {
        // SAFETY: `self.raw` is live for this callback.
        unsafe {
            MediaFrame::from_raw(sys::switch_core_media_bug_get_native_read_frame(
                self.raw.as_ptr(),
            ))
        }
    }

    pub fn native_write_frame(&self) -> Option<MediaFrame<'_>> {
        // SAFETY: `self.raw` is live for this callback.
        unsafe {
            MediaFrame::from_raw(sys::switch_core_media_bug_get_native_write_frame(
                self.raw.as_ptr(),
            ))
        }
    }

    pub fn read_replace_frame(&mut self) -> Option<MediaFrameMut<'_>> {
        // SAFETY: `self.raw` is live for this callback.
        unsafe {
            MediaFrameMut::from_raw(sys::switch_core_media_bug_get_read_replace_frame(
                self.raw.as_ptr(),
            ))
        }
    }

    pub fn write_replace_frame(&mut self) -> Option<MediaFrameMut<'_>> {
        // SAFETY: `self.raw` is live for this callback.
        unsafe {
            MediaFrameMut::from_raw(sys::switch_core_media_bug_get_write_replace_frame(
                self.raw.as_ptr(),
            ))
        }
    }

    pub fn set_read_replace_frame(&mut self, frame: &mut MediaFrameMut<'_>) {
        // SAFETY: Both pointers are live for this callback.
        unsafe {
            sys::switch_core_media_bug_set_read_replace_frame(self.raw.as_ptr(), frame.as_ptr());
        }
    }

    pub fn set_write_replace_frame(&mut self, frame: &mut MediaFrameMut<'_>) {
        // SAFETY: Both pointers are live for this callback.
        unsafe {
            sys::switch_core_media_bug_set_write_replace_frame(self.raw.as_ptr(), frame.as_ptr());
        }
    }

    pub fn flush(&mut self) {
        // SAFETY: `self.raw` is live for this callback.
        unsafe {
            sys::switch_core_media_bug_flush(self.raw.as_ptr());
        }
    }

    pub fn read_into(&mut self, frame: &mut sys::switch_frame_t, fill: bool) -> Result<()> {
        let fill = if fill {
            sys::switch_bool_t_SWITCH_TRUE
        } else {
            sys::switch_bool_t_SWITCH_FALSE
        };
        // SAFETY: `self.raw` is live and `frame` is caller-provided writable frame storage.
        let status = unsafe { sys::switch_core_media_bug_read(self.raw.as_ptr(), frame, fill) };
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
        unsafe { self.raw.as_ref().datalen as usize }
    }

    pub fn samples(self) -> u32 {
        // SAFETY: `self.raw` is live for this frame wrapper.
        unsafe { self.raw.as_ref().samples }
    }

    pub fn rate(self) -> u32 {
        // SAFETY: `self.raw` is live for this frame wrapper.
        unsafe { self.raw.as_ref().rate }
    }

    pub fn channels(self) -> u32 {
        // SAFETY: `self.raw` is live for this frame wrapper.
        unsafe { self.raw.as_ref().channels }
    }

    pub fn bytes(self) -> &'a [u8] {
        // SAFETY: `self.raw` is live and FreeSWITCH keeps `data` valid for `datalen` bytes during
        // the callback. Null data with zero length is represented as an empty slice.
        unsafe {
            let frame = self.raw.as_ref();
            if frame.data.is_null() || frame.datalen == 0 {
                &[]
            } else {
                slice::from_raw_parts(frame.data.cast::<u8>(), frame.datalen as usize)
            }
        }
    }

    pub fn pcm_i16(self) -> Option<&'a [i16]> {
        let bytes = self.bytes();
        if !bytes.len().is_multiple_of(std::mem::size_of::<i16>())
            || !(bytes.as_ptr() as usize).is_multiple_of(std::mem::align_of::<i16>())
        {
            return None;
        }

        // SAFETY: Length and alignment were checked above.
        Some(unsafe {
            slice::from_raw_parts(bytes.as_ptr().cast::<i16>(), bytes.len() / size_of::<i16>())
        })
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
        unsafe {
            let frame = self.raw.as_ref();
            if frame.data.is_null() || frame.datalen == 0 {
                &mut []
            } else {
                slice::from_raw_parts_mut(frame.data.cast::<u8>(), frame.datalen as usize)
            }
        }
    }

    pub fn pcm_i16_mut(&mut self) -> Option<&mut [i16]> {
        let bytes = self.bytes_mut();
        if !bytes.len().is_multiple_of(std::mem::size_of::<i16>())
            || !(bytes.as_ptr() as usize).is_multiple_of(std::mem::align_of::<i16>())
        {
            return None;
        }

        // SAFETY: Length and alignment were checked above, and `bytes` is uniquely borrowed.
        Some(unsafe {
            slice::from_raw_parts_mut(
                bytes.as_mut_ptr().cast::<i16>(),
                bytes.len() / size_of::<i16>(),
            )
        })
    }
}

struct MediaBugState<H> {
    handler: H,
}

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

    let Some(mut ctx) = (unsafe { MediaBugContext::from_raw(bug) }) else {
        return sys::switch_bool_t_SWITCH_TRUE;
    };

    if callback_type == sys::switch_abc_type_t_SWITCH_ABC_TYPE_CLOSE {
        // SAFETY: Close is the terminal callback for the pointer passed to FreeSWITCH.
        let mut state = unsafe { Box::from_raw(user_data.cast::<MediaBugState<H>>()) };
        let result = catch_unwind(AssertUnwindSafe(|| {
            state.handler.on_close(&mut ctx);
            MediaBugAction::Continue
        }));
        return callback_result(result);
    }

    // SAFETY: `user_data` is the `MediaBugState<H>` pointer passed to `switch_core_media_bug_add`.
    let state = unsafe { &mut *user_data.cast::<MediaBugState<H>>() };
    let result = catch_unwind(AssertUnwindSafe(|| {
        if callback_type == sys::switch_abc_type_t_SWITCH_ABC_TYPE_INIT {
            state.handler.on_init(&mut ctx)
        } else if callback_type == sys::switch_abc_type_t_SWITCH_ABC_TYPE_READ {
            // SAFETY: `bug` is live for the callback duration.
            let frame = unsafe {
                MediaFrame::from_raw(sys::switch_core_media_bug_get_native_read_frame(bug))
            };
            match frame {
                Some(frame) => state.handler.on_read(&mut ctx, frame),
                None => MediaBugAction::Continue,
            }
        } else if callback_type == sys::switch_abc_type_t_SWITCH_ABC_TYPE_WRITE {
            // SAFETY: `bug` is live for the callback duration.
            let frame = unsafe {
                MediaFrame::from_raw(sys::switch_core_media_bug_get_native_write_frame(bug))
            };
            match frame {
                Some(frame) => state.handler.on_write(&mut ctx, frame),
                None => MediaBugAction::Continue,
            }
        } else if callback_type == sys::switch_abc_type_t_SWITCH_ABC_TYPE_READ_REPLACE {
            // SAFETY: `bug` is live for the callback duration.
            let frame = unsafe {
                MediaFrameMut::from_raw(sys::switch_core_media_bug_get_read_replace_frame(bug))
            };
            match frame {
                Some(frame) => state.handler.on_read_replace(&mut ctx, frame),
                None => MediaBugAction::Continue,
            }
        } else if callback_type == sys::switch_abc_type_t_SWITCH_ABC_TYPE_WRITE_REPLACE {
            // SAFETY: `bug` is live for the callback duration.
            let frame = unsafe {
                MediaFrameMut::from_raw(sys::switch_core_media_bug_get_write_replace_frame(bug))
            };
            match frame {
                Some(frame) => state.handler.on_write_replace(&mut ctx, frame),
                None => MediaBugAction::Continue,
            }
        } else {
            MediaBugAction::Continue
        }
    }));

    callback_result(result)
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
