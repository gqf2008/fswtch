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
