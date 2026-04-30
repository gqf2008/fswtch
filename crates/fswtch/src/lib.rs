#![allow(clippy::not_unsafe_ptr_arg_deref)]

pub use fswtch_sys as sys;

use std::{
    error::Error,
    ffi::{CStr, c_char},
    fmt,
    ptr::NonNull,
};

pub type Status = sys::switch_status_t;

pub const SUCCESS: Status = sys::switch_status_t::SWITCH_STATUS_SUCCESS;
pub const FALSE: Status = sys::switch_status_t::SWITCH_STATUS_FALSE;
pub const GENERR: Status = sys::switch_status_t::SWITCH_STATUS_GENERR;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct SwitchError(pub Status);

impl fmt::Display for SwitchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FreeSWITCH returned status {:?}", self.0)
    }
}

impl Error for SwitchError {}

pub type Result<T> = std::result::Result<T, SwitchError>;

pub fn status_to_result(status: Status) -> Result<()> {
    if status == SUCCESS {
        Ok(())
    } else {
        Err(SwitchError(status))
    }
}

pub fn log_example(module: &str, message: impl fmt::Display) {
    eprintln!("[fswtch:{module}] {message}");
}

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

pub struct Stream {
    raw: NonNull<sys::switch_stream_handle_t>,
}

impl Stream {
    /// Wraps a FreeSWITCH stream handle.
    ///
    /// # Safety
    ///
    /// `raw` must be a valid stream handle for the current FreeSWITCH callback invocation.
    pub unsafe fn from_raw(raw: *mut sys::switch_stream_handle_t) -> Option<Self> {
        NonNull::new(raw).map(|raw| Self { raw })
    }

    pub fn as_ptr(&self) -> *mut sys::switch_stream_handle_t {
        self.raw.as_ptr()
    }

    pub fn write_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        let raw = self.raw.as_ptr();
        // SAFETY: `self.raw` is guaranteed valid by `Stream::from_raw`'s caller contract.
        let Some(write) = (unsafe { &*raw }).raw_write_function else {
            return Err(SwitchError(GENERR));
        };

        // SAFETY: FreeSWITCH's stream writer accepts the stream handle and a byte buffer valid for
        // the duration of the call.
        let status = unsafe { write(raw, bytes.as_ptr().cast_mut(), bytes.len()) };
        status_to_result(status)
    }

    pub fn write_str(&mut self, text: &str) -> Result<()> {
        self.write_bytes(text.as_bytes())
    }
}

#[macro_export]
macro_rules! module_exports {
    (
        module = $module:ident,
        load = $load:path $(,)?
    ) => {
        $crate::module_exports! {
            module = $module,
            load = $load,
            shutdown = None,
            runtime = None,
        }
    };
    (
        module = $module:ident,
        load = $load:path,
        shutdown = $shutdown:expr,
        runtime = $runtime:expr $(,)?
    ) => {
        #[unsafe(export_name = concat!(stringify!($module), "_module_interface"))]
        pub static mut SWITCH_RUST_MODULE_INTERFACE:
            $crate::sys::switch_loadable_module_function_table_t =
            $crate::sys::switch_loadable_module_function_table_t {
                switch_api_version: $crate::sys::SWITCH_API_VERSION as _,
                load: Some($load),
                shutdown: $shutdown,
                runtime: $runtime,
                flags: 0,
            };
    };
}
