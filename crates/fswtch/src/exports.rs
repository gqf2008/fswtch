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
        // Declares the exported FreeSWITCH module-interface table. The static's type is
        // `$crate::__ModuleFunctionTable` — a `#[repr(transparent)]` wrapper over FreeSWITCH's
        // `switch_loadable_module_function_table_t` — so the downstream crate never names a
        // `*-sys` type. `__new` takes the load/shutdown/runtime callbacks as `extern "C" fn(...) ->
        // fswtch::Status` (repr-transparent over `switch_status_t`, ABI-identical) and bridges
        // them into the C function-table layout internally. `shutdown`/`runtime` are
        // `Option<extern "C" fn() -> fswtch::Status>` — module authors write `-> fswtch::Status`
        // and never touch the raw status enum.
        #[unsafe(export_name = concat!(stringify!($module), "_module_interface"))]
        pub static mut SWITCH_RUST_MODULE_INTERFACE: $crate::__ModuleFunctionTable =
            // SAFETY: `__new` is `unsafe` only because it `transmute`s the `Status`-returning /
            // `c_void`-typed callbacks into the `switch_status_t`-returning C function-pointer
            // field layout; the `module_load!` trampoline + user `-> fswtch::Status` fns satisfy
            // the ABI-compatibility contract. `__new` is `const`, so this runs at compile time in
            // the static initializer.
            unsafe { $crate::__ModuleFunctionTable::__new(Some($load), $shutdown, $runtime) };
    };
}
