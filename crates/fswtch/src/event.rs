use std::{
    ffi::{CStr, CString},
    ptr::NonNull,
};

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

    /// Serializes the event into a compact binary representation (malloc'd by FreeSWITCH).
    ///
    /// The returned buffer can later be rebuilt into an [`Event`] with [`binary_deserialize`].
    pub fn binary_serialize(&self) -> Result<Vec<u8>> {
        let Some(raw) = self.raw else {
            return Ok(Vec::new());
        };
        let mut data: *mut std::ffi::c_void = std::ptr::null_mut();
        let mut len: sys::switch_size_t = 0;
        // SAFETY: `raw` is a live event; `data` and `len` are writable output storage. FreeSWITCH
        // malloc's the buffer, which we copy out and free below.
        let status =
            unsafe { sys::switch_event_binary_serialize(raw.as_ptr(), &mut data, &mut len) };
        status_to_result(status)?;
        if data.is_null() {
            return Ok(Vec::new());
        }
        // SAFETY: FreeSWITCH allocated `len` bytes at `data` with a malloc-family allocator.
        let bytes = unsafe {
            let slice = std::slice::from_raw_parts(data as *const u8, len);
            slice.to_vec()
        };
        // SAFETY: `data` was malloc'd by FreeSWITCH and is now copied out.
        unsafe { crate::free_cstr(data as *mut std::ffi::c_char) };
        Ok(bytes)
    }

    /// Copies the channel's presence-data columns onto this event under `prefix`.
    ///
    /// Wraps `switch_event_add_presence_data_cols`. The channel is read-only for this call.
    pub fn add_presence_data_cols(&mut self, channel: crate::Channel, prefix: impl AsRef<str>) {
        let Some(raw) = self.raw else {
            return;
        };
        let Ok(prefix) = cstring(prefix) else {
            return;
        };
        // SAFETY: `raw` is a live event, `channel` is a live channel, and `prefix` is a valid C
        // string. The call only reads from the channel and writes headers into the event.
        unsafe {
            sys::switch_event_add_presence_data_cols(
                channel.as_ptr(),
                raw.as_ptr(),
                prefix.as_ptr(),
            );
        }
    }

    /// Returns `true` when `name` is permitted by the permission `list` event.
    ///
    /// Wraps `switch_event_check_permission_list`. A nonzero return from FreeSWITCH is treated as
    /// "permitted". This is a read-only lookup against an event shaped like a permission list.
    pub fn check_permission_list(&self, name: impl AsRef<str>) -> bool {
        let Some(raw) = self.raw else {
            return false;
        };
        let Ok(name) = cstring(name) else {
            return false;
        };
        // SAFETY: `raw` is a live event used as a permission list; `name` is a valid C string.
        let permitted =
            unsafe { sys::switch_event_check_permission_list(raw.as_ptr(), name.as_ptr()) };
        permitted != 0
    }

    /// Adds a header without copying the value into the event's pool.
    ///
    /// Unlike [`add_header`](Self::add_header), the C macro `switch_event_add_header_string_nodup`
    /// **adopts** the value buffer (it does not copy it). This wrapper leaks the CString's allocation
    /// so FreeSWITCH takes ownership without a double-free when the CString would otherwise drop.
    /// Prefer [`add_header`](Self::add_header) for normal use (it copies, so no leak).
    pub fn add_header_nodup(&mut self, name: impl AsRef<str>, value: &str) -> Result<()> {
        let Some(raw) = self.raw else {
            return Ok(());
        };
        let name = cstring(name)?;
        let value = cstring(value)?;
        // Leak the value's allocation so FreeSWITCH adopts it (nodup = no copy). The name is
        // copied internally by FreeSWITCH, so it does not need to be leaked.
        let value_ptr = Box::leak(value.into_boxed_c_str()).as_ptr();
        // SAFETY: `raw` is a live event; `name` and `value_ptr` are valid C strings. FreeSWITCH
        // adopts `value_ptr` (does not free it from the caller's side).
        let status = unsafe {
            sys::switch_event_add_header_string_nodup(
                raw.as_ptr(),
                sys::switch_stack_t::SWITCH_STACK_BOTTOM,
                name.as_ptr(),
                value_ptr,
            )
        };
        status_to_result(status)
    }

    /// Adds a header using the variadic `switch_event_add_header` entry point with a `%s` format.
    ///
    /// For normal string values this is equivalent to [`add_header`](Self::add_header); it exercises
    /// the printf-based code path. Free-form format strings are not exposed because safe Rust cannot
    /// soundly build a C variadic argument list.
    pub fn add_header_fmt(&mut self, name: impl AsRef<str>, value: &str) -> Result<()> {
        let Some(raw) = self.raw else {
            return Ok(());
        };
        let name = cstring(name)?;
        let value = cstring(value)?;
        // SAFETY: `raw` is a live event; the format string and `value` are valid C strings. The
        // variadic call supplies the single `%s` argument.
        let status = unsafe {
            sys::switch_event_add_header(
                raw.as_ptr(),
                sys::switch_stack_t::SWITCH_STACK_BOTTOM,
                name.as_ptr(),
                c"%s".as_ptr(),
                value.as_ptr(),
            )
        };
        status_to_result(status)
    }

    /// Adds an array-valued header (a header that may carry multiple indexed values).
    ///
    /// Wraps `switch_event_add_array`. Returns the resulting index count on success.
    pub fn add_array(&mut self, name: impl AsRef<str>, value: &str) -> Result<i32> {
        let Some(raw) = self.raw else {
            return Ok(0);
        };
        let name = cstring(name)?;
        let value = cstring(value)?;
        // SAFETY: `raw` is a live event and both C strings are valid for this call.
        let count =
            unsafe { sys::switch_event_add_array(raw.as_ptr(), name.as_ptr(), value.as_ptr()) };
        if count < 0 {
            Err(crate::SwitchError(crate::GENERR))
        } else {
            Ok(count)
        }
    }

    /// Renames every header named `old_name` to `new_name`.
    ///
    /// Wraps `switch_event_rename_header`.
    pub fn rename_header(
        &mut self,
        old_name: impl AsRef<str>,
        new_name: impl AsRef<str>,
    ) -> Result<()> {
        let Some(raw) = self.raw else {
            return Ok(());
        };
        let old_name = cstring(old_name)?;
        let new_name = cstring(new_name)?;
        // SAFETY: `raw` is a live event and both C strings are valid for this call.
        let status = unsafe {
            sys::switch_event_rename_header(raw.as_ptr(), old_name.as_ptr(), new_name.as_ptr())
        };
        status_to_result(status)
    }

    /// Sets (or, with an empty `name`, clears) the custom subclass name of this event.
    ///
    /// Wraps `switch_event_set_subclass_name`.
    pub fn set_subclass_name(&mut self, name: impl AsRef<str>) -> Result<()> {
        let Some(raw) = self.raw else {
            return Ok(());
        };
        let name = cstring(name)?;
        // SAFETY: `raw` is a live event and `name` is a valid C string.
        let status = unsafe { sys::switch_event_set_subclass_name(raw.as_ptr(), name.as_ptr()) };
        status_to_result(status)
    }

    /// Sets the delivery priority of this event.
    ///
    /// Wraps `switch_event_set_priority`. Pass one of the
    /// `switch_priority_t_SWITCH_PRIORITY_{NORMAL,LOW,HIGH}` constants.
    pub fn set_priority(&mut self, priority: sys::switch_priority_t) -> Result<()> {
        let Some(raw) = self.raw else {
            return Ok(());
        };
        // SAFETY: `raw` is a live event.
        let status = unsafe { sys::switch_event_set_priority(raw.as_ptr(), priority) };
        status_to_result(status)
    }

    /// Replaces the event body with `body`.
    ///
    /// Wraps `switch_event_set_body`. Use [`add_body`](Self::add_body) to append instead of replace.
    pub fn set_body(&mut self, body: &str) -> Result<()> {
        let Some(raw) = self.raw else {
            return Ok(());
        };
        let body = cstring(body)?;
        // SAFETY: `raw` is a live event and `body` is a valid C string.
        let status = unsafe { sys::switch_event_set_body(raw.as_ptr(), body.as_ptr()) };
        status_to_result(status)
    }

    /// Deep-copies this event into a new owned [`Event`].
    ///
    /// Wraps `switch_event_dup`.
    pub fn dup(&self) -> Result<Event> {
        let Some(raw) = self.raw else {
            return Ok(Event { raw: None });
        };
        let mut out: *mut sys::switch_event_t = std::ptr::null_mut();
        // SAFETY: `raw` is a live event; `out` is writable output storage. FreeSWITCH allocates a
        // fresh event into `out` on success.
        let status = unsafe { sys::switch_event_dup(&mut out, raw.as_ptr()) };
        status_to_result(status)?;
        Ok(Event {
            raw: NonNull::new(out),
        })
    }

    /// Like [`dup`](Self::dup) but marks the copy as a reply (clears delivery metadata).
    ///
    /// Wraps `switch_event_dup_reply`.
    pub fn dup_reply(&self) -> Result<Event> {
        let Some(raw) = self.raw else {
            return Ok(Event { raw: None });
        };
        let mut out: *mut sys::switch_event_t = std::ptr::null_mut();
        // SAFETY: `raw` is a live event; `out` is writable output storage.
        let status = unsafe { sys::switch_event_dup_reply(&mut out, raw.as_ptr()) };
        status_to_result(status)?;
        Ok(Event {
            raw: NonNull::new(out),
        })
    }

    /// Merges headers from `other` into this event, adding only headers not already present.
    ///
    /// Wraps `switch_event_merge`. The `other` event is read-only for this call.
    pub fn merge(&mut self, other: &Event) {
        let (Some(raw), Some(other_raw)) = (self.raw, other.raw) else {
            return;
        };
        // SAFETY: both pointers are live events; `tomerge` is read-only.
        unsafe { sys::switch_event_merge(raw.as_ptr(), other_raw.as_ptr()) };
    }

    /// Serializes the event to a flat `Key: Value\n` string.
    ///
    /// Wraps `switch_event_serialize`. When `encode` is true the values are URL-encoded.
    pub fn serialize(&self, encode: bool) -> Result<String> {
        let Some(raw) = self.raw else {
            return Ok(String::new());
        };
        let mut out: *mut std::ffi::c_char = std::ptr::null_mut();
        // SAFETY: `raw` is a live event; `out` is writable output storage receiving a malloc'd
        // string. `encode` selects URL-encoding of values.
        let status =
            unsafe { sys::switch_event_serialize(raw.as_ptr(), &mut out, encode_as_bool(encode)) };
        status_to_result(status)?;
        // SAFETY: On success `out` is a malloc'd null-terminated string owned by this call.
        Ok(unsafe { crate::strdup_to_string(out) }.unwrap_or_default())
    }

    /// Serializes the event to a JSON string.
    ///
    /// Wraps `switch_event_serialize_json`.
    pub fn to_json(&self) -> Result<String> {
        let Some(raw) = self.raw else {
            return Ok(String::new());
        };
        let mut out: *mut std::ffi::c_char = std::ptr::null_mut();
        // SAFETY: `raw` is a live event; `out` is writable output storage receiving a malloc'd
        // JSON string.
        let status = unsafe { sys::switch_event_serialize_json(raw.as_ptr(), &mut out) };
        status_to_result(status)?;
        // SAFETY: On success `out` is a malloc'd null-terminated string owned by this call.
        Ok(unsafe { crate::strdup_to_string(out) }.unwrap_or_default())
    }

    /// Serializes the event into an existing cJSON object.
    ///
    /// Wraps `switch_event_serialize_json_obj`. **Escape hatch:** `obj` is a raw `*mut *mut
    /// sys::cJSON` because this crate does not expose a safe cJSON builder; construct the object
    /// with FreeSWITCH's JSON helpers and pass its address here.
    pub fn to_json_obj(&self, obj: *mut *mut sys::cJSON) -> Result<()> {
        let Some(raw) = self.raw else {
            return Ok(());
        };
        // SAFETY: `raw` is a live event; `obj` is a valid cJSON handle provided by the caller.
        let status = unsafe { sys::switch_event_serialize_json_obj(raw.as_ptr(), obj) };
        status_to_result(status)
    }

    /// Renders the event as an XML document.
    ///
    /// Wraps `switch_event_xmlize`. The returned [`EventXml`] owns the document and frees it on
    /// drop. The C entry point is variadic; the safe wrapper passes an empty format string and no
    /// varargs, which produces a plain `<event>` document with no extra root formatting.
    pub fn xmlize(&self) -> Result<EventXml> {
        let Some(raw) = self.raw else {
            return Ok(EventXml { raw: None });
        };
        // SAFETY: `raw` is a live event. An empty format string requests no extra formatting of the
        // root node; no variadic arguments are supplied.
        let xml = unsafe { sys::switch_event_xmlize(raw.as_ptr(), c"".as_ptr()) };
        Ok(EventXml {
            raw: NonNull::new(xml),
        })
    }

    /// Expands `${var}` references in `value` using this event's headers.
    ///
    /// Wraps `switch_event_expand_headers_check`. Returns the expanded string. Variable and API
    /// lookup tables are not supplied (callers needing them should use the raw API), so only
    /// headers on this event are consulted. When no expansion occurs the input is returned
    /// unchanged.
    pub fn expand_headers(&self, value: &str) -> Result<String> {
        let Some(raw) = self.raw else {
            return Ok(value.to_owned());
        };
        let value = cstring(value)?;
        let in_ptr = value.as_ptr();
        // SAFETY: `raw` is a live event and `value` is a valid C string. Null var/api lists disable
        // variable-list and API expansion; recursion depth zero matches the common case.
        let out = unsafe {
            sys::switch_event_expand_headers_check(
                raw.as_ptr(),
                in_ptr,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
            )
        };
        if out.is_null() {
            return Ok(value.to_string_lossy().into_owned());
        }
        // switch_event_expand_headers_check returns the ORIGINAL input pointer when no ${}
        // expansion occurs — freeing it would double-free the CString's buffer. Only call
        // strdup_to_string (which frees) when the pointer differs from the input.
        if out == in_ptr.cast_mut() {
            return Ok(value.to_string_lossy().into_owned());
        }
        // SAFETY: `out` is a freshly malloc'd null-terminated string owned by this call.
        Ok(unsafe { crate::strdup_to_string(out) }
            .unwrap_or_else(|| value.to_string_lossy().into_owned()))
    }

    /// Parses a JSON string into a new owned [`Event`].
    ///
    /// Wraps `switch_event_create_json`.
    pub fn from_json(json: &str) -> Result<Event> {
        let json = cstring(json)?;
        let mut out: *mut sys::switch_event_t = std::ptr::null_mut();
        // SAFETY: `json` is a valid C string; `out` is writable output storage. FreeSWITCH parses
        // the JSON and allocates an event into `out` on success.
        let status = unsafe { sys::switch_event_create_json(&mut out, json.as_ptr()) };
        status_to_result(status)?;
        Ok(Event {
            raw: NonNull::new(out),
        })
    }

    /// Builds an event from a bracket-delimited `name=value` string.
    ///
    /// Wraps `switch_event_create_brackets`. `open`/`close`/`escape` select the bracket characters;
    /// the portion of `data` inside the brackets is consumed into headers and the unconsumed tail
    /// is returned as the second element of the tuple. When `dup` is true the parsed values are
    /// duplicated into the event rather than referencing the input buffer.
    ///
    /// FreeSWITCH walks `data` with a mutable cursor, so the safe wrapper copies `data` into a
    /// fresh nul-terminated buffer that may be safely mutated for the duration of the call.
    pub fn create_brackets(
        data: &str,
        open: char,
        close: char,
        escape: char,
        dup: bool,
    ) -> Result<(Event, String)> {
        // Build a mutable, nul-terminated buffer FreeSWITCH may advance in place.
        let mut bytes: Vec<u8> = data.as_bytes().to_vec();
        bytes.push(0);
        let mut event: *mut sys::switch_event_t = std::ptr::null_mut();
        let mut new_data: *mut std::ffi::c_char = std::ptr::null_mut();
        // SAFETY: `bytes` is a valid nul-terminated C string for the duration of this call and may
        // be mutated by FreeSWITCH; `event` and `new_data` are writable output storage.
        let status = unsafe {
            sys::switch_event_create_brackets(
                bytes.as_mut_ptr().cast(),
                open as std::ffi::c_char,
                close as std::ffi::c_char,
                escape as std::ffi::c_char,
                &mut event,
                &mut new_data,
                encode_as_bool(dup),
            )
        };
        status_to_result(status)?;
        // SAFETY: On success `new_data` is a malloc'd null-terminated string owned by this call.
        let remaining = unsafe { crate::strdup_to_string(new_data) }.unwrap_or_default();
        Ok((
            Event {
                raw: NonNull::new(event),
            },
            remaining,
        ))
    }

    /// Builds an event from parallel `names`/`vals` slices.
    ///
    /// Wraps `switch_event_create_array_pair`. The slices must have equal length; each pair becomes
    /// a header on the new event.
    pub fn create_array_pair(names: &[&str], vals: &[&str]) -> Result<Event> {
        if names.len() != vals.len() {
            return Err(crate::SwitchError(crate::GENERR));
        }
        let name_cstrs: Vec<CString> = names
            .iter()
            .map(|name| cstring(*name))
            .collect::<Result<_>>()?;
        let val_cstrs: Vec<CString> = vals
            .iter()
            .map(|val| cstring(*val))
            .collect::<Result<_>>()?;
        // Keep the backing CStrings alive across the FFI call.
        let mut name_ptrs: Vec<*mut std::ffi::c_char> =
            name_cstrs.iter().map(|c| c.as_ptr() as *mut _).collect();
        let mut val_ptrs: Vec<*mut std::ffi::c_char> =
            val_cstrs.iter().map(|c| c.as_ptr() as *mut _).collect();
        let mut event: *mut sys::switch_event_t = std::ptr::null_mut();
        // SAFETY: `name_ptrs`/`val_ptrs` are arrays of valid C string pointers whose backing
        // `CString`s outlive the call; `event` is writable output storage.
        let status = unsafe {
            sys::switch_event_create_array_pair(
                &mut event,
                name_ptrs.as_mut_ptr(),
                val_ptrs.as_mut_ptr(),
                name_ptrs.len() as std::ffi::c_int,
            )
        };
        status_to_result(status)?;
        Ok(Event {
            raw: NonNull::new(event),
        })
    }
}

/// Converts a Rust `bool` to FreeSWITCH's `switch_bool_t`.
fn encode_as_bool(value: bool) -> sys::switch_bool_t {
    if value {
        sys::switch_bool_t_SWITCH_TRUE
    } else {
        sys::switch_bool_t_SWITCH_FALSE
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

    /// Returns the value of the `idx`-th header named `name`, or the last when `idx` is negative.
    ///
    /// Wraps `switch_event_get_header_idx` (the same call backing [`header`](Self::header), which
    /// always reads the last value).
    pub fn header_idx(self, name: impl AsRef<str>, idx: i32) -> Option<String> {
        let raw = self.raw?;
        let name = cstring(name).ok()?;
        // SAFETY: `raw` is a live event for the callback duration.
        let value = unsafe { sys::switch_event_get_header_idx(raw.as_ptr(), name.as_ptr(), idx) };
        // SAFETY: FreeSWITCH returns a null-terminated header value when present.
        unsafe { CStr::from_ptr(value) }
            .to_str()
            .ok()
            .map(ToOwned::to_owned)
    }

    /// Borrows the event body for the callback's duration, when present.
    ///
    /// Wraps `switch_event_get_body`. The returned `&str` borrows the event's own storage and is
    /// valid only while the [`EventRef`] remains live.
    pub fn body_str(self) -> Option<&'static str> {
        let raw = self.raw?;
        // SAFETY: `raw` is a live event for the callback duration; the returned pointer borrows the
        // event's storage.
        let ptr = unsafe { sys::switch_event_get_body(raw.as_ptr()) };
        // SAFETY: The pointer is null or a valid C string borrowing the event body. The lifetime is
        // bounded by the `EventRef`, which outlives this borrow; `'static` is the conventional
        // escape-hatch lifetime used by the crate's other borrowed-string accessors.
        unsafe { crate::borrowed_cstr_to_str(ptr) }
    }

    /// Iterates over every `(name, value)` header on this event.
    ///
    /// Walks the `headers` linked list of the underlying `switch_event_t`. The yielded strings
    /// borrow the event's storage and are valid only while the [`EventRef`] remains live.
    pub fn headers(&self) -> HeaderIter<'_> {
        let raw = self.raw.map_or(std::ptr::null_mut(), |n| n.as_ptr());
        // SAFETY: `raw` is null or a live event pointer for the callback duration. Reading the
        // `headers` field of a live event is safe.
        let head = if raw.is_null() {
            std::ptr::null_mut()
        } else {
            // SAFETY: `raw` is non-null and live.
            unsafe { (*raw).headers }
        };
        HeaderIter {
            current: head,
            _marker: std::marker::PhantomData,
        }
    }

    /// Serializes the event to a flat `Key: Value\n` string (read-only on the borrowed event).
    ///
    /// Wraps `switch_event_serialize`. When `encode` is true the values are URL-encoded.
    pub fn serialize(self, encode: bool) -> Result<String> {
        let raw = self.raw.map_or(std::ptr::null_mut(), |n| n.as_ptr());
        if raw.is_null() {
            return Ok(String::new());
        }
        let mut out: *mut std::ffi::c_char = std::ptr::null_mut();
        // SAFETY: `raw` is a live event; `out` is writable output storage receiving a malloc'd
        // string.
        let status = unsafe { sys::switch_event_serialize(raw, &mut out, encode_as_bool(encode)) };
        status_to_result(status)?;
        // SAFETY: On success `out` is a malloc'd null-terminated string owned by this call.
        Ok(unsafe { crate::strdup_to_string(out) }.unwrap_or_default())
    }

    /// Serializes the event to a JSON string (read-only on the borrowed event).
    ///
    /// Wraps `switch_event_serialize_json`.
    pub fn to_json(self) -> Result<String> {
        let raw = self.raw.map_or(std::ptr::null_mut(), |n| n.as_ptr());
        if raw.is_null() {
            return Ok(String::new());
        }
        let mut out: *mut std::ffi::c_char = std::ptr::null_mut();
        // SAFETY: `raw` is a live event; `out` is writable output storage receiving a malloc'd
        // JSON string.
        let status = unsafe { sys::switch_event_serialize_json(raw, &mut out) };
        status_to_result(status)?;
        // SAFETY: On success `out` is a malloc'd null-terminated string owned by this call.
        Ok(unsafe { crate::strdup_to_string(out) }.unwrap_or_default())
    }

    /// Expands `${var}` references in `value` using this event's headers (read-only).
    ///
    /// Wraps `switch_event_expand_headers_check`. See [`Event::expand_headers`] for caveats.
    pub fn expand_headers(self, value: &str) -> Result<String> {
        let raw = self.raw.map_or(std::ptr::null_mut(), |n| n.as_ptr());
        if raw.is_null() {
            return Ok(value.to_owned());
        }
        let value = cstring(value)?;
        let in_ptr = value.as_ptr();
        // SAFETY: `raw` is a live event and `value` is a valid C string.
        let out = unsafe {
            sys::switch_event_expand_headers_check(
                raw,
                in_ptr,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
            )
        };
        if out.is_null() {
            return Ok(value.to_string_lossy().into_owned());
        }
        // switch_event_expand_headers_check returns the input pointer when no expansion occurs —
        // freeing it would double-free the CString's buffer. Only free when it differs.
        if out == in_ptr.cast_mut() {
            return Ok(value.to_string_lossy().into_owned());
        }
        // SAFETY: `out` is a freshly malloc'd null-terminated string owned by this call.
        Ok(unsafe { crate::strdup_to_string(out) }
            .unwrap_or_else(|| value.to_string_lossy().into_owned()))
    }
}

/// Iterator over the `(name, value)` headers of an [`EventRef`].
pub struct HeaderIter<'a> {
    current: *mut sys::switch_event_header_t,
    _marker: std::marker::PhantomData<&'a sys::switch_event_t>,
}

impl<'a> Iterator for HeaderIter<'a> {
    type Item = (&'a str, &'a str);

    fn next(&mut self) -> Option<Self::Item> {
        if self.current.is_null() {
            return None;
        }
        // SAFETY: `current` is non-null and points to a live header node in the event's list.
        let header = unsafe { &*self.current };
        // SAFETY: `name` and `value` borrow the event's storage for the iterator's lifetime.
        let name = unsafe { crate::borrowed_cstr_to_str(header.name as *const std::ffi::c_char) }?;
        let value = unsafe { crate::borrowed_cstr_to_str(header.value as *const std::ffi::c_char) }
            .unwrap_or("");
        self.current = header.next;
        Some((name, value))
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

/// Registers a permanent (non-removable) event subscription.
///
/// Wraps `switch_event_bind`, the variant of [`EventBinder::bind`] that does not return a node
/// handle. Because there is no node to unbind, the registration lives for the lifetime of the
/// FreeSWITCH event subsystem and **cannot be revoked** from safe Rust. Prefer
/// [`EventBinder::bind`] when you need to unbind later.
///
/// `callback` is the C trampoline FreeSWITCH will invoke (typically generated with the
/// `event_callback!` macro); `user_data` is stored on the node but is not passed back to the
/// callback (see [`EventBinder`] docs).
pub fn bind_permanent(
    id: impl AsRef<str>,
    event: sys::switch_event_types_t,
    subclass: Option<&str>,
    callback: sys::switch_event_callback_t,
    user_data: *mut std::ffi::c_void,
) -> Result<()> {
    let id = cstring(id)?;
    let subclass = match subclass {
        Some(text) => Some(cstring(text)?),
        None => None,
    };
    // SAFETY: `id` and `subclass` are valid C strings; the callback matches the expected ABI.
    let status = unsafe {
        sys::switch_event_bind(
            id.as_ptr(),
            event,
            subclass
                .as_ref()
                .map_or(std::ptr::null(), |subclass| subclass.as_ptr()),
            callback,
            user_data,
        )
    };
    status_to_result(status)
}

/// Removes every subscription registered with the given `callback`.
///
/// Wraps `switch_event_unbind_callback`, which unbinds by callback function pointer rather than
/// by node handle. This is the counterpart to [`bind_permanent`] for callers that did not receive
/// a node.
pub fn unbind_callback(callback: sys::switch_event_callback_t) -> Result<()> {
    // SAFETY: `callback` is the function pointer originally passed to `switch_event_bind`.
    let status = unsafe { sys::switch_event_unbind_callback(callback) };
    status_to_result(status)
}

/// Rebuilds an [`Event`] from a buffer produced by [`Event::binary_serialize`].
///
/// Wraps `switch_event_binary_deserialize`. The returned [`Event`] owns the new event and
/// destroys it on drop. `duplicate` controls whether FreeSWITCH copies the input buffer
/// (`SWITCH_TRUE`) or references it; passing `true` keeps the safe caller's `data` borrow
/// short-lived and is the default in the convenience wrapper below.
pub fn binary_deserialize(data: &[u8]) -> Result<Event> {
    if data.is_empty() {
        return Ok(Event { raw: None });
    }
    let mut raw: *mut sys::switch_event_t = std::ptr::null_mut();
    // SAFETY: `raw` is writable output storage; `data_ptr`/`len` describe the caller's buffer.
    // We pass `SWITCH_TRUE` so FreeSWITCH duplicates the buffer and does not retain the borrow.
    let mut data_ptr = data.as_ptr() as *mut std::ffi::c_void;
    let status = unsafe {
        sys::switch_event_binary_deserialize(
            &mut raw,
            &mut data_ptr,
            data.len() as sys::switch_size_t,
            sys::switch_bool_t_SWITCH_TRUE,
        )
    };
    status_to_result(status)?;
    Ok(Event {
        raw: NonNull::new(raw),
    })
}

/// Subscribes a callback to a named event channel (the FreeSWITCH "event channel" pub/sub bus,
/// distinct from [`EventBinder`]/[`bind_permanent`] which subscribe to typed `switch_event_t`s).
///
/// Wraps `switch_event_channel_bind`. The assigned channel id is returned so the caller can pass
/// it to [`channel_broadcast`] / [`channel_deliver`]. Because `switch_event_channel_unbind`
/// matches on the callback + `user_data` pair (not an opaque handle), there is no RAII guard
/// here; call [`channel_unbind`] with the same callback and `user_data` to unregister.
///
/// `func` is the C trampoline FreeSWITCH invokes when a JSON message is delivered to the channel
/// (see `sys::switch_event_channel_func_t`).
pub fn channel_bind(
    event_channel: impl AsRef<str>,
    func: sys::switch_event_channel_func_t,
    user_data: *mut std::ffi::c_void,
) -> Result<sys::switch_event_channel_id_t> {
    let event_channel = cstring(event_channel)?;
    let mut id: sys::switch_event_channel_id_t = 0;
    // SAFETY: `event_channel` is a valid C string; `id` is writable output storage.
    let status =
        unsafe { sys::switch_event_channel_bind(event_channel.as_ptr(), func, &mut id, user_data) };
    status_to_result(status)?;
    Ok(id)
}

/// Removes every subscription on `event_channel` matching `func` and `user_data`.
///
/// Wraps `switch_event_channel_unbind`, returning the number of subscriptions removed.
pub fn channel_unbind(
    event_channel: impl AsRef<str>,
    func: sys::switch_event_channel_func_t,
    user_data: *mut std::ffi::c_void,
) -> u32 {
    let Ok(event_channel) = cstring(event_channel) else {
        return 0;
    };
    // SAFETY: `event_channel` is a valid C string; `func`/`user_data` match a prior bind.
    unsafe { sys::switch_event_channel_unbind(event_channel.as_ptr(), func, user_data) }
}

/// Broadcasts a JSON message onto a named event channel.
///
/// Wraps `switch_event_channel_broadcast`. **Escape hatch:** `json` is a raw `*mut *mut
/// sys::cJSON` because this crate does not expose a safe cJSON builder; construct the cJSON
/// object with the underlying FreeSWITCH JSON helpers and pass its address here. FreeSWITCH
/// consumes the cJSON on success.
pub fn channel_broadcast(
    event_channel: impl AsRef<str>,
    json: *mut *mut sys::cJSON,
    key: impl AsRef<str>,
    id: sys::switch_event_channel_id_t,
) -> Result<()> {
    let event_channel = cstring(event_channel)?;
    let key = cstring(key)?;
    // SAFETY: `event_channel` and `key` are valid C strings; `json` is a valid cJSON handle.
    let status = unsafe {
        sys::switch_event_channel_broadcast(event_channel.as_ptr(), json, key.as_ptr(), id)
    };
    status_to_result(status)
}

/// Delivers a JSON message to the subscribers of a named event channel (like
/// [`channel_broadcast`] but scoped to a single delivery).
///
/// Wraps `switch_event_channel_deliver`. **Escape hatch:** `json` is a raw `*mut *mut
/// sys::cJSON` because this crate does not expose a safe cJSON builder.
pub fn channel_deliver(
    event_channel: impl AsRef<str>,
    json: *mut *mut sys::cJSON,
    key: impl AsRef<str>,
    id: sys::switch_event_channel_id_t,
) -> Result<()> {
    let event_channel = cstring(event_channel)?;
    let key = cstring(key)?;
    // SAFETY: `event_channel` and `key` are valid C strings; `json` is a valid cJSON handle.
    let status = unsafe {
        sys::switch_event_channel_deliver(event_channel.as_ptr(), json, key.as_ptr(), id)
    };
    status_to_result(status)
}

/// Returns `true` when `cookie` is permitted to publish on `event_channel`.
///
/// Wraps `switch_event_channel_permission_verify`.
pub fn channel_permission_verify(cookie: impl AsRef<str>, event_channel: impl AsRef<str>) -> bool {
    let Ok(cookie) = cstring(cookie) else {
        return false;
    };
    let Ok(event_channel) = cstring(event_channel) else {
        return false;
    };
    // SAFETY: `cookie` and `event_channel` are valid C strings; the call is a read-only lookup.
    let allowed = unsafe {
        sys::switch_event_channel_permission_verify(cookie.as_ptr(), event_channel.as_ptr())
    };
    allowed == sys::switch_bool_t_SWITCH_TRUE
}

/// Grants (`set` = `true`) or revokes (`set` = `false`) permission for `cookie` to publish on
/// `event_channel`.
///
/// Wraps `switch_event_channel_permission_modify`.
pub fn channel_permission_modify(
    cookie: impl AsRef<str>,
    event_channel: impl AsRef<str>,
    set: bool,
) {
    let Ok(cookie) = cstring(cookie) else {
        return;
    };
    let Ok(event_channel) = cstring(event_channel) else {
        return;
    };
    // SAFETY: `cookie` and `event_channel` are valid C strings.
    unsafe {
        sys::switch_event_channel_permission_modify(
            cookie.as_ptr(),
            event_channel.as_ptr(),
            if set {
                sys::switch_bool_t_SWITCH_TRUE
            } else {
                sys::switch_bool_t_SWITCH_FALSE
            },
        );
    }
}

/// Clears every permission entry for `cookie`.
///
/// Wraps `switch_event_channel_permission_clear`.
pub fn channel_permission_clear(cookie: impl AsRef<str>) {
    let Ok(cookie) = cstring(cookie) else {
        return;
    };
    // SAFETY: `cookie` is a valid C string.
    unsafe { sys::switch_event_channel_permission_clear(cookie.as_ptr()) };
}

/// Owned XML document produced by [`Event::xmlize`].
///
/// The document is freed (`switch_xml_free`) when this guard is dropped.
pub struct EventXml {
    raw: Option<NonNull<sys::switch_xml>>,
}

impl EventXml {
    /// The raw `switch_xml_t` handle, for advanced use with the XML API.
    pub fn as_ptr(&self) -> sys::switch_xml_t {
        self.raw.map_or(std::ptr::null_mut(), NonNull::as_ptr)
    }
}

impl Drop for EventXml {
    fn drop(&mut self) {
        if let Some(xml) = self.raw.take() {
            // SAFETY: `xml` was returned by `switch_event_xmlize` and is owned by this guard.
            unsafe { sys::switch_xml_free(xml.as_ptr()) };
        }
    }
}

/// Returns the canonical name of an event type, or `None` when the type is unknown.
///
/// Wraps `switch_event_name`. The returned string is static storage owned by FreeSWITCH.
pub fn event_name(event_type: sys::switch_event_types_t) -> Option<&'static str> {
    // SAFETY: `switch_event_name` returns either a static C string or NULL; it reads no caller
    // state and performs no allocation.
    let ptr = unsafe { sys::switch_event_name(event_type) };
    // SAFETY: The pointer is null or a static null-terminated C string.
    unsafe { crate::borrowed_cstr_to_str(ptr) }
}

/// Parses a canonical event name into its `switch_event_types_t` value.
///
/// Wraps `switch_name_event`. Returns `None` when the name does not match a known event type.
pub fn name_event(name: impl AsRef<str>) -> Option<sys::switch_event_types_t> {
    let name = cstring(name).ok()?;
    let mut out: sys::switch_event_types_t = sys::switch_event_types_t::SWITCH_EVENT_CUSTOM;
    // SAFETY: `name` is a valid C string; `out` is writable output storage.
    let status = unsafe { sys::switch_name_event(name.as_ptr(), &mut out) };
    if status == crate::SUCCESS {
        Some(out)
    } else {
        None
    }
}

/// Returns `true` when the FreeSWITCH event subsystem is running.
///
/// Wraps `switch_event_running` (a status code is treated as a boolean: success = running).
pub fn event_running() -> bool {
    // SAFETY: `switch_event_running` takes no arguments and performs a read-only check.
    let status = unsafe { sys::switch_event_running() };
    status == crate::SUCCESS
}
