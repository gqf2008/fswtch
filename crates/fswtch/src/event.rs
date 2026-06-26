use std::{ffi::CStr, ptr::NonNull};

use crate::command::borrowed_cstr_to_string;
use crate::{Result, cstring, status_to_result, sys};

macro_rules! call_ffi {
    ($call:expr) => {{
        // SAFETY: The caller documents the FreeSWITCH ABI preconditions at each call site.
        unsafe { $call }
    }};
}

pub struct Event {
    raw: Option<NonNull<sys::switch_event_t>>,
}

impl Event {
    pub fn custom(subclass: impl AsRef<str>) -> Result<Self> {
        let subclass = cstring(subclass)?;
        let mut raw = std::ptr::null_mut();
        // SAFETY: FreeSWITCH initializes `raw` for the custom subclass when the call succeeds.
        let status = unsafe {
            create_event(
                &mut raw,
                sys::switch_event_types_t::SWITCH_EVENT_CUSTOM,
                Some(subclass.as_c_str()),
            )
        };
        status_to_result(status)?;
        Ok(Self {
            raw: NonNull::new(raw),
        })
    }

    /// Creates a non-custom event of the given type (e.g. `SWITCH_EVENT_CHANNEL_CREATE`).
    pub fn new(event: sys::switch_event_types_t) -> Result<Self> {
        let mut raw = std::ptr::null_mut();
        // SAFETY: FreeSWITCH initializes `raw` for the event type when the call succeeds.
        let status = unsafe { create_event(&mut raw, event, None) };
        status_to_result(status)?;
        Ok(Self {
            raw: NonNull::new(raw),
        })
    }

    pub fn as_ptr(&self) -> *mut sys::switch_event_t {
        self.raw.map_or(std::ptr::null_mut(), NonNull::as_ptr)
    }

    pub fn add_header(&mut self, name: impl AsRef<str>, value: &str) -> Result<()> {
        let Some(raw) = self.raw else {
            return Ok(());
        };
        let name = cstring(name)?;
        let value = cstring(value)?;
        // SAFETY: `raw` is a live event and both C strings are valid for this call.
        let status = unsafe {
            sys::switch_event_add_header_string(
                raw.as_ptr(),
                sys::switch_stack_t::SWITCH_STACK_BOTTOM,
                name.as_ptr(),
                value.as_ptr(),
            )
        };
        status_to_result(status)
    }

    pub fn add_header_name(&mut self, name: impl AsRef<str>, value: &str) -> Result<()> {
        self.add_header(name, value)
    }

    /// Appends a body string to the event.
    pub fn add_body(&mut self, body: &str) -> Result<()> {
        let Some(raw) = self.raw else {
            return Ok(());
        };
        let body = cstring(body)?;
        // SAFETY: `raw` is a live event; the format string and `body` are valid C strings. The
        // variadic call supplies the single `%s` argument.
        let status =
            unsafe { sys::switch_event_add_body(raw.as_ptr(), c"%s".as_ptr(), body.as_ptr()) };
        status_to_result(status)
    }

    /// Deletes every header named `name` regardless of value.
    pub fn del_header(&mut self, name: impl AsRef<str>) -> Result<()> {
        let Some(raw) = self.raw else {
            return Ok(());
        };
        let name = cstring(name)?;
        // SAFETY: `raw` is a live event; `name` is a valid C string; a null `val` deletes any value.
        let status = unsafe {
            sys::switch_event_del_header_val(raw.as_ptr(), name.as_ptr(), std::ptr::null())
        };
        status_to_result(status)
    }

    /// The event body, when present.
    pub fn body(&self) -> Option<String> {
        let raw = self.raw?;
        // SAFETY: `raw` is a live event.
        let ptr = unsafe { sys::switch_event_get_body(raw.as_ptr()) };
        borrowed_cstr_to_string(ptr)
    }

    pub fn fire(mut self) -> Result<()> {
        let Some(raw) = self.raw.take() else {
            return Ok(());
        };
        let mut raw = raw.as_ptr();
        // SAFETY: Ownership of `raw` transfers to FreeSWITCH on successful fire.
        let status = unsafe { fire_event(&mut raw) };
        if status == crate::SUCCESS {
            Ok(())
        } else {
            self.raw = NonNull::new(raw);
            Err(crate::SwitchError(status))
        }
    }
}

/// # Safety
///
/// `raw` must be writable output storage; `subclass`, when `Some`, must be a live C string.
// SAFETY: The caller must provide writable event output storage and, when supplied, a live subclass.
unsafe fn create_event(
    raw: &mut *mut sys::switch_event_t,
    event: sys::switch_event_types_t,
    subclass: Option<&CStr>,
) -> sys::switch_status_t {
    let create = sys::switch_event_create_subclass_detailed;
    call_ffi!(create(
        c"fswtch-rs".as_ptr(),
        c"Event::create".as_ptr(),
        line!() as _,
        raw,
        event,
        subclass.map_or(std::ptr::null(), CStr::as_ptr),
    ))
}

/// # Safety
///
/// `raw` must point to an owned event pointer. FreeSWITCH takes ownership on success.
// SAFETY: The caller must provide an owned event pointer for FreeSWITCH to fire.
unsafe fn fire_event(raw: &mut *mut sys::switch_event_t) -> sys::switch_status_t {
    let fire = sys::switch_event_fire_detailed;
    call_ffi!(fire(
        c"fswtch-rs".as_ptr(),
        c"Event::fire".as_ptr(),
        line!() as _,
        raw,
        std::ptr::null_mut(),
    ))
}

impl Drop for Event {
    fn drop(&mut self) {
        if let Some(raw) = self.raw.take() {
            let mut raw = raw.as_ptr();
            // SAFETY: The event is still owned by this wrapper because `fire` was not called.
            unsafe {
                sys::switch_event_destroy(&mut raw);
            }
        }
    }
}

#[derive(Copy, Clone)]
pub struct EventRef {
    raw: Option<NonNull<sys::switch_event_t>>,
}

impl EventRef {
    /// Wraps a FreeSWITCH event pointer for the duration of a callback.
    ///
    /// # Safety
    ///
    /// `raw` must point to a live FreeSWITCH event and remain valid while this wrapper is used.
    pub unsafe fn from_raw(raw: *mut sys::switch_event_t) -> Self {
        Self {
            raw: NonNull::new(raw),
        }
    }

    pub fn as_ptr(self) -> *mut sys::switch_event_t {
        self.raw.map_or(std::ptr::null_mut(), NonNull::as_ptr)
    }

    pub fn header(self, name: impl AsRef<str>) -> Option<String> {
        let raw = self.raw?;
        let name = cstring(name).ok()?;
        // SAFETY: `raw` is a live event for the callback duration.
        let value = unsafe { sys::switch_event_get_header_idx(raw.as_ptr(), name.as_ptr(), -1) };
        if value.is_null() {
            return None;
        }

        // SAFETY: FreeSWITCH returns a null-terminated header value when present.
        unsafe { CStr::from_ptr(value) }
            .to_str()
            .ok()
            .map(ToOwned::to_owned)
    }
}

/// RAII guard for a FreeSWITCH event subscription registered via `switch_event_bind_removable`.
///
/// The subscription is removed (`switch_event_unbind`) when this guard is dropped.
///
/// Note: FreeSWITCH's event callback signature receives only the event — no `user_data`. A boxed
/// Rust closure therefore cannot be recovered through the callback alone; supply the C callback
/// yourself (typically generated with the `event_callback!` macro) and thread any state through
/// module-level storage, mirroring how native FreeSWITCH modules handle event callbacks.
pub struct EventBinder {
    node: Option<NonNull<sys::switch_event_node_t>>,
}

impl EventBinder {
    /// Subscribes to `event` (and, for custom events, the named `subclass`) under `id`.
    ///
    /// `callback` is the C trampoline FreeSWITCH will invoke; `user_data` is stored on the node but
    /// is not passed back to the callback (see the type docs). Returns a guard whose `Drop`
    /// unregisters the subscription.
    pub fn bind(
        id: impl AsRef<str>,
        event: sys::switch_event_types_t,
        subclass: Option<&str>,
        callback: sys::switch_event_callback_t,
        user_data: *mut std::ffi::c_void,
    ) -> Result<Self> {
        let id = cstring(id)?;
        let subclass = match subclass {
            Some(text) => Some(cstring(text)?),
            None => None,
        };
        let mut node: *mut sys::switch_event_node_t = std::ptr::null_mut();
        // SAFETY: `id` and `subclass` are valid C strings; `node` is writable output storage.
        let status = unsafe {
            sys::switch_event_bind_removable(
                id.as_ptr(),
                event,
                subclass
                    .as_ref()
                    .map_or(std::ptr::null(), |subclass| subclass.as_ptr()),
                callback,
                user_data,
                &mut node,
            )
        };
        status_to_result(status)?;
        Ok(Self {
            node: NonNull::new(node),
        })
    }

    /// The registration handle, for advanced use with the raw event API.
    pub fn as_ptr(&self) -> *mut sys::switch_event_node_t {
        self.node.map_or(std::ptr::null_mut(), NonNull::as_ptr)
    }
}

impl Drop for EventBinder {
    fn drop(&mut self) {
        if let Some(node) = self.node.take() {
            let mut node = node.as_ptr();
            // SAFETY: `node` is the registration handle obtained from `switch_event_bind_removable`.
            unsafe { sys::switch_event_unbind(&mut node) };
        }
    }
}
