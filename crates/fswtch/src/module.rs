use std::{
    ffi::{CStr, c_char},
    ptr::NonNull,
};

use crate::{
    GENERR, Result, Status, SwitchError,
    sys::{self},
};

#[derive(Copy, Clone)]
pub struct Module {
    raw: NonNull<sys::switch_loadable_module_interface_t>,
}

impl Module {
    /// Creates the FreeSWITCH module interface for a load callback.
    ///
    /// # Safety
    ///
    /// `slot` and `pool` must be the values passed to the module load callback by FreeSWITCH.
    /// `name` must remain valid for the lifetime of the loaded module.
    pub unsafe fn create(
        slot: *mut *mut sys::switch_loadable_module_interface_t,
        pool: *mut sys::switch_memory_pool_t,
        name: &'static CStr,
    ) -> Result<Self> {
        if slot.is_null() {
            return Err(SwitchError(GENERR));
        }

        // SAFETY: The caller guarantees `pool` and `name` are valid for FreeSWITCH's loader.
        let raw =
            unsafe { sys::switch_loadable_module_create_module_interface(pool, name.as_ptr()) };
        let raw = NonNull::new(raw).ok_or(SwitchError(GENERR))?;
        // SAFETY: `slot` was checked for null above and points to FreeSWITCH's output slot.
        unsafe {
            *slot = raw.as_ptr();
        }
        Ok(Self { raw })
    }

    pub fn as_ptr(&self) -> *mut sys::switch_loadable_module_interface_t {
        self.raw.as_ptr()
    }

    /// Registers a FreeSWITCH API command on this module.
    ///
    /// # Safety
    ///
    /// The provided `function` must obey FreeSWITCH's `switch_api_function_t` ABI. The C strings
    /// must remain valid for the lifetime of the loaded module.
    pub unsafe fn add_api(
        self,
        name: &'static CStr,
        description: &'static CStr,
        syntax: &'static CStr,
        function: unsafe extern "C" fn(
            *const c_char,
            *mut sys::switch_core_session_t,
            *mut sys::switch_stream_handle_t,
        ) -> Status,
    ) -> Result<ApiInterface> {
        // SAFETY: `self.raw` is a live module interface created by FreeSWITCH for this module.
        let raw = unsafe {
            sys::switch_loadable_module_create_interface(
                self.raw.as_ptr(),
                sys::switch_module_interface_name_t::SWITCH_API_INTERFACE,
            )
        };
        let api =
            NonNull::new(raw.cast::<sys::switch_api_interface_t>()).ok_or(SwitchError(GENERR))?;

        // SAFETY: `api` is a valid API interface allocation returned by FreeSWITCH, and all
        // assigned C string/function pointers have static lifetimes.
        unsafe {
            let api_ref = api.as_ptr();
            (*api_ref).interface_name = name.as_ptr();
            (*api_ref).desc = description.as_ptr();
            (*api_ref).function = Some(function);
            (*api_ref).syntax = syntax.as_ptr();
        }

        Ok(ApiInterface { raw: api })
    }
}

#[derive(Copy, Clone)]
pub struct ApiInterface {
    raw: NonNull<sys::switch_api_interface_t>,
}

impl ApiInterface {
    pub fn as_ptr(&self) -> *mut sys::switch_api_interface_t {
        self.raw.as_ptr()
    }
}
