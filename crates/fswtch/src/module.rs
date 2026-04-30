use std::{ffi::c_char, ptr::NonNull};

use crate::{
    GENERR, Result, StaticCStr, Status, SwitchError,
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
    /// `slot` and `pool` must be the live loader-owned pointers passed by FreeSWITCH to this
    /// module's load callback. `slot` must be writable for one module interface pointer.
    pub unsafe fn create(
        slot: *mut *mut sys::switch_loadable_module_interface_t,
        pool: *mut sys::switch_memory_pool_t,
        name: impl StaticCStr,
    ) -> Result<Self> {
        if slot.is_null() {
            return Err(SwitchError(GENERR));
        }
        let name = name.into_static_cstr()?;

        // SAFETY: The caller guarantees `pool` and `slot` are FreeSWITCH loader-owned pointers.
        let raw = unsafe {
            let raw = sys::switch_loadable_module_create_module_interface(pool, name.as_ptr());
            let raw = NonNull::new(raw).ok_or(SwitchError(GENERR))?;
            *slot = raw.as_ptr();
            raw
        };
        Ok(Self { raw })
    }

    pub fn as_ptr(&self) -> *mut sys::switch_loadable_module_interface_t {
        self.raw.as_ptr()
    }

    /// Registers a FreeSWITCH API command on this module.
    pub fn add_api(
        self,
        name: impl StaticCStr,
        description: impl StaticCStr,
        syntax: impl StaticCStr,
        function: unsafe extern "C" fn(
            *const c_char,
            *mut sys::switch_core_session_t,
            *mut sys::switch_stream_handle_t,
        ) -> Status,
    ) -> Result<ApiInterface> {
        let name = name.into_static_cstr()?;
        let description = description.into_static_cstr()?;
        let syntax = syntax.into_static_cstr()?;
        let api = create_interface::<sys::switch_api_interface_t>(
            self.raw,
            sys::switch_module_interface_name_t::SWITCH_API_INTERFACE,
        )?;

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
        info: ApplicationInfo,
        function: unsafe extern "C" fn(*mut sys::switch_core_session_t, *const c_char),
    ) -> Result<ApplicationInterface> {
        let strings = info.into_cstrings()?;
        let application = create_interface::<sys::switch_application_interface_t>(
            self.raw,
            sys::switch_module_interface_name_t::SWITCH_APPLICATION_INTERFACE,
        )?;

        // SAFETY: `application` is a valid interface allocation returned by FreeSWITCH, and all
        // assigned C string/function pointers have static lifetimes.
        unsafe {
            let application_ref = application.as_ptr();
            (*application_ref).interface_name = strings.name.as_ptr();
            (*application_ref).application_function = Some(function);
            (*application_ref).long_desc = strings.long_description.as_ptr();
            (*application_ref).short_desc = strings.short_description.as_ptr();
            (*application_ref).syntax = strings.syntax.as_ptr();
        }

        Ok(ApplicationInterface { raw: application })
    }

    pub fn add_chat_application(
        self,
        info: ApplicationInfo,
        function: unsafe extern "C" fn(*mut sys::switch_event_t, *const c_char) -> Status,
    ) -> Result<ChatApplicationInterface> {
        let strings = info.into_cstrings()?;
        let application = create_interface::<sys::switch_chat_application_interface_t>(
            self.raw,
            sys::switch_module_interface_name_t::SWITCH_CHAT_APPLICATION_INTERFACE,
        )?;

        // SAFETY: `application` is a valid interface allocation returned by FreeSWITCH, and all
        // assigned C string/function pointers have static lifetimes.
        unsafe {
            let application_ref = application.as_ptr();
            (*application_ref).interface_name = strings.name.as_ptr();
            (*application_ref).chat_application_function = Some(function);
            (*application_ref).long_desc = strings.long_description.as_ptr();
            (*application_ref).short_desc = strings.short_description.as_ptr();
            (*application_ref).syntax = strings.syntax.as_ptr();
        }

        Ok(ChatApplicationInterface { raw: application })
    }

    pub fn add_endpoint(
        self,
        name: impl StaticCStr,
        io_routines: *mut sys::switch_io_routines_t,
    ) -> Result<EndpointInterface> {
        let name = name.into_static_cstr()?;
        let endpoint = create_interface::<sys::switch_endpoint_interface_t>(
            self.raw,
            sys::switch_module_interface_name_t::SWITCH_ENDPOINT_INTERFACE,
        )?;

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

fn create_interface<T>(
    module: NonNull<sys::switch_loadable_module_interface_t>,
    kind: sys::switch_module_interface_name_t,
) -> Result<NonNull<T>> {
    // SAFETY: `module` is a live module interface created by FreeSWITCH for this module.
    let raw = unsafe { sys::switch_loadable_module_create_interface(module.as_ptr(), kind) };
    NonNull::new(raw.cast::<T>()).ok_or(SwitchError(GENERR))
}

#[derive(Copy, Clone)]
pub struct ApplicationInfo {
    pub name: &'static str,
    pub long_description: &'static str,
    pub short_description: &'static str,
    pub syntax: &'static str,
}

impl ApplicationInfo {
    pub const fn new(
        name: &'static str,
        long_description: &'static str,
        short_description: &'static str,
        syntax: &'static str,
    ) -> Self {
        Self {
            name,
            long_description,
            short_description,
            syntax,
        }
    }

    fn into_cstrings(self) -> Result<ApplicationInfoCStrings> {
        Ok(ApplicationInfoCStrings {
            name: self.name.into_static_cstr()?,
            long_description: self.long_description.into_static_cstr()?,
            short_description: self.short_description.into_static_cstr()?,
            syntax: self.syntax.into_static_cstr()?,
        })
    }
}

struct ApplicationInfoCStrings {
    name: &'static std::ffi::CStr,
    long_description: &'static std::ffi::CStr,
    short_description: &'static std::ffi::CStr,
    syntax: &'static std::ffi::CStr,
}

pub struct ModuleBuilder {
    module: Module,
}

impl ModuleBuilder {
    /// Creates a module registration builder from FreeSWITCH load callback pointers.
    ///
    /// # Safety
    ///
    /// `slot` and `pool` must be the live loader-owned pointers passed by FreeSWITCH to this
    /// module's load callback. `slot` must be writable for one module interface pointer.
    pub unsafe fn new(
        slot: *mut *mut sys::switch_loadable_module_interface_t,
        pool: *mut sys::switch_memory_pool_t,
        name: impl StaticCStr,
    ) -> Result<Self> {
        Ok(Self {
            // SAFETY: Forwarded from `ModuleBuilder::new`'s caller.
            module: unsafe { Module::create(slot, pool, name)? },
        })
    }

    pub fn api(
        self,
        name: impl StaticCStr,
        description: impl StaticCStr,
        syntax: impl StaticCStr,
        function: unsafe extern "C" fn(
            *const c_char,
            *mut sys::switch_core_session_t,
            *mut sys::switch_stream_handle_t,
        ) -> Status,
    ) -> Result<Self> {
        self.module.add_api(name, description, syntax, function)?;
        Ok(self)
    }

    pub fn application(
        self,
        info: ApplicationInfo,
        function: unsafe extern "C" fn(*mut sys::switch_core_session_t, *const c_char),
    ) -> Result<Self> {
        self.module.add_application(info, function)?;
        Ok(self)
    }

    pub fn chat_application(
        self,
        info: ApplicationInfo,
        function: unsafe extern "C" fn(*mut sys::switch_event_t, *const c_char) -> Status,
    ) -> Result<Self> {
        self.module.add_chat_application(info, function)?;
        Ok(self)
    }

    pub fn endpoint(
        self,
        name: impl StaticCStr,
        io_routines: *mut sys::switch_io_routines_t,
    ) -> Result<Self> {
        self.module.add_endpoint(name, io_routines)?;
        Ok(self)
    }

    pub fn finish(self) -> Module {
        self.module
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
