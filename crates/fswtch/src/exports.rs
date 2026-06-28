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
                shutdown: {
                    // Trampoline: the user supplies `shutdown` as
                    // `Option<extern "C" fn() -> fswtch::Status>`; FreeSWITCH's function table
                    // wants `Option<extern "C" fn() -> sys::switch_status_t>`. We bridge the
                    // newtype here so module authors write `-> fswtch::Status` and never touch
                    // `sys` at the FFI boundary.
                    extern "C" fn __fswtch_shutdown_wrap() -> $crate::sys::switch_status_t {
                        // `let` (not `const`) so fn-item → fn-pointer coercion applies.
                        let user: Option<extern "C" fn() -> $crate::Status> = $shutdown;
                        match user {
                            Some(f) => f().raw(),
                            None => $crate::sys::switch_status_t::SWITCH_STATUS_FALSE,
                        }
                    }
                    Some(__fswtch_shutdown_wrap)
                },
                runtime: $runtime,
                flags: 0,
            };
    };
}
