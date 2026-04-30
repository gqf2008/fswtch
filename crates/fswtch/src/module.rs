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
    pub fn create(
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
    pub fn add_api(
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

    pub fn add_application(
        self,
        name: &'static CStr,
        long_description: &'static CStr,
        short_description: &'static CStr,
        syntax: &'static CStr,
        function: unsafe extern "C" fn(*mut sys::switch_core_session_t, *const c_char),
    ) -> Result<ApplicationInterface> {
        // SAFETY: `self.raw` is a live module interface created by FreeSWITCH for this module.
        let raw = unsafe {
            sys::switch_loadable_module_create_interface(
                self.raw.as_ptr(),
                sys::switch_module_interface_name_t::SWITCH_APPLICATION_INTERFACE,
            )
        };
        let application = NonNull::new(raw.cast::<sys::switch_application_interface_t>())
            .ok_or(SwitchError(GENERR))?;

        // SAFETY: `application` is a valid interface allocation returned by FreeSWITCH, and all
        // assigned C string/function pointers have static lifetimes.
        unsafe {
            let application_ref = application.as_ptr();
            (*application_ref).interface_name = name.as_ptr();
            (*application_ref).application_function = Some(function);
            (*application_ref).long_desc = long_description.as_ptr();
            (*application_ref).short_desc = short_description.as_ptr();
            (*application_ref).syntax = syntax.as_ptr();
        }

        Ok(ApplicationInterface { raw: application })
    }

    pub fn add_chat_application(
        self,
        name: &'static CStr,
        long_description: &'static CStr,
        short_description: &'static CStr,
        syntax: &'static CStr,
        function: unsafe extern "C" fn(*mut sys::switch_event_t, *const c_char) -> Status,
    ) -> Result<ChatApplicationInterface> {
        // SAFETY: `self.raw` is a live module interface created by FreeSWITCH for this module.
        let raw = unsafe {
            sys::switch_loadable_module_create_interface(
                self.raw.as_ptr(),
                sys::switch_module_interface_name_t::SWITCH_CHAT_APPLICATION_INTERFACE,
            )
        };
        let application = NonNull::new(raw.cast::<sys::switch_chat_application_interface_t>())
            .ok_or(SwitchError(GENERR))?;

        // SAFETY: `application` is a valid interface allocation returned by FreeSWITCH, and all
        // assigned C string/function pointers have static lifetimes.
        unsafe {
            let application_ref = application.as_ptr();
            (*application_ref).interface_name = name.as_ptr();
            (*application_ref).chat_application_function = Some(function);
            (*application_ref).long_desc = long_description.as_ptr();
            (*application_ref).short_desc = short_description.as_ptr();
            (*application_ref).syntax = syntax.as_ptr();
        }

        Ok(ChatApplicationInterface { raw: application })
    }

    pub fn add_endpoint(
        self,
        name: &'static CStr,
        io_routines: *mut sys::switch_io_routines_t,
    ) -> Result<EndpointInterface> {
        // SAFETY: `self.raw` is a live module interface created by FreeSWITCH for this module.
        let raw = unsafe {
            sys::switch_loadable_module_create_interface(
                self.raw.as_ptr(),
                sys::switch_module_interface_name_t::SWITCH_ENDPOINT_INTERFACE,
            )
        };
        let endpoint = NonNull::new(raw.cast::<sys::switch_endpoint_interface_t>())
            .ok_or(SwitchError(GENERR))?;

        // SAFETY: `endpoint` is a valid interface allocation returned by FreeSWITCH. `name` has a
        // static lifetime, and the caller supplies module-owned I/O routine storage.
        unsafe {
            let endpoint_ref = endpoint.as_ptr();
            (*endpoint_ref).interface_name = name.as_ptr();
            (*endpoint_ref).io_routines = io_routines;
        }

        Ok(EndpointInterface { raw: endpoint })
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

#[derive(Copy, Clone)]
pub struct ApplicationInterface {
    raw: NonNull<sys::switch_application_interface_t>,
}

impl ApplicationInterface {
    pub fn as_ptr(&self) -> *mut sys::switch_application_interface_t {
        self.raw.as_ptr()
    }
}

#[derive(Copy, Clone)]
pub struct ChatApplicationInterface {
    raw: NonNull<sys::switch_chat_application_interface_t>,
}

impl ChatApplicationInterface {
    pub fn as_ptr(&self) -> *mut sys::switch_chat_application_interface_t {
        self.raw.as_ptr()
    }
}

#[derive(Copy, Clone)]
pub struct EndpointInterface {
    raw: NonNull<sys::switch_endpoint_interface_t>,
}

impl EndpointInterface {
    pub fn as_ptr(&self) -> *mut sys::switch_endpoint_interface_t {
        self.raw.as_ptr()
    }
}
