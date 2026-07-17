use std::{ffi::c_char, ptr::NonNull};

use crate::{
    GENERR, Result, StaticCStr, SwitchError,
    sys::{self},
};

// Callback type aliases for interfaces whose fields are inline `Option<fn>` (no bindgen typedef).
pub(crate) type ChatSendFn =
    Option<unsafe extern "C" fn(message_event: *mut sys::switch_event_t) -> sys::switch_status_t>;
pub(crate) type ManagementFn = Option<
    unsafe extern "C" fn(
        relative_oid: *mut c_char,
        action: sys::switch_management_action_t,
        data: *mut c_char,
        datalen: sys::switch_size_t,
    ) -> sys::switch_status_t,
>;
pub(crate) type LimitIncrFn = Option<
    unsafe extern "C" fn(
        session: *mut sys::switch_core_session_t,
        realm: *const c_char,
        resource: *const c_char,
        max: std::os::raw::c_int,
        interval: std::os::raw::c_int,
    ) -> sys::switch_status_t,
>;
pub(crate) type LimitReleaseFn = Option<
    unsafe extern "C" fn(
        session: *mut sys::switch_core_session_t,
        realm: *const c_char,
        resource: *const c_char,
    ) -> sys::switch_status_t,
>;
pub(crate) type LimitUsageFn = Option<
    unsafe extern "C" fn(
        realm: *const c_char,
        resource: *const c_char,
        rcount: *mut u32,
    ) -> std::os::raw::c_int,
>;
pub(crate) type LimitResetFn = Option<unsafe extern "C" fn() -> sys::switch_status_t>;
pub(crate) type LimitStatusFn = Option<unsafe extern "C" fn() -> *mut c_char>;
pub(crate) type LimitIntervalResetFn = Option<
    unsafe extern "C" fn(realm: *const c_char, resource: *const c_char) -> sys::switch_status_t,
>;
pub(crate) type TimerInitFn =
    Option<unsafe extern "C" fn(arg1: *mut sys::switch_timer_t) -> sys::switch_status_t>;
pub(crate) type TimerNextFn =
    Option<unsafe extern "C" fn(arg1: *mut sys::switch_timer_t) -> sys::switch_status_t>;
pub(crate) type TimerStepFn =
    Option<unsafe extern "C" fn(arg1: *mut sys::switch_timer_t) -> sys::switch_status_t>;
pub(crate) type TimerSyncFn =
    Option<unsafe extern "C" fn(arg1: *mut sys::switch_timer_t) -> sys::switch_status_t>;
pub(crate) type TimerCheckFn = Option<
    unsafe extern "C" fn(
        arg1: *mut sys::switch_timer_t,
        arg2: sys::switch_bool_t,
    ) -> sys::switch_status_t,
>;
pub(crate) type TimerDestroyFn =
    Option<unsafe extern "C" fn(arg1: *mut sys::switch_timer_t) -> sys::switch_status_t>;
pub(crate) type FileOpenFn = Option<
    unsafe extern "C" fn(
        arg1: *mut sys::switch_file_handle_t,
        file_path: *const c_char,
    ) -> sys::switch_status_t,
>;
pub(crate) type FileCloseFn =
    Option<unsafe extern "C" fn(arg1: *mut sys::switch_file_handle_t) -> sys::switch_status_t>;
pub(crate) type FileTruncateFn = Option<
    unsafe extern "C" fn(arg1: *mut sys::switch_file_handle_t, offset: i64) -> sys::switch_status_t,
>;
pub(crate) type FileReadFn = Option<
    unsafe extern "C" fn(
        arg1: *mut sys::switch_file_handle_t,
        data: *mut std::ffi::c_void,
        len: *mut sys::switch_size_t,
    ) -> sys::switch_status_t,
>;
pub(crate) type FileWriteFn = Option<
    unsafe extern "C" fn(
        arg1: *mut sys::switch_file_handle_t,
        data: *mut std::ffi::c_void,
        len: *mut sys::switch_size_t,
    ) -> sys::switch_status_t,
>;
pub(crate) type SpeechOpenFn = Option<
    unsafe extern "C" fn(
        sh: *mut sys::switch_speech_handle_t,
        voice_name: *const c_char,
        rate: std::os::raw::c_int,
        channels: std::os::raw::c_int,
        flags: *mut sys::switch_speech_flag_t,
    ) -> sys::switch_status_t,
>;
pub(crate) type SpeechCloseFn = Option<
    unsafe extern "C" fn(
        arg1: *mut sys::switch_speech_handle_t,
        flags: *mut sys::switch_speech_flag_t,
    ) -> sys::switch_status_t,
>;
pub(crate) type SpeechFeedTtsFn = Option<
    unsafe extern "C" fn(
        sh: *mut sys::switch_speech_handle_t,
        text: *mut c_char,
        flags: *mut sys::switch_speech_flag_t,
    ) -> sys::switch_status_t,
>;
pub(crate) type SpeechReadTtsFn = Option<
    unsafe extern "C" fn(
        sh: *mut sys::switch_speech_handle_t,
        data: *mut std::ffi::c_void,
        datalen: *mut sys::switch_size_t,
        flags: *mut sys::switch_speech_flag_t,
    ) -> sys::switch_status_t,
>;
pub(crate) type AsrOpenFn = Option<
    unsafe extern "C" fn(
        ah: *mut sys::switch_asr_handle_t,
        codec: *const c_char,
        rate: std::os::raw::c_int,
        dest: *const c_char,
        flags: *mut sys::switch_asr_flag_t,
    ) -> sys::switch_status_t,
>;
pub(crate) type AsrLoadGrammarFn = Option<
    unsafe extern "C" fn(
        ah: *mut sys::switch_asr_handle_t,
        grammar: *const c_char,
        name: *const c_char,
    ) -> sys::switch_status_t,
>;
pub(crate) type AsrUnloadGrammarFn = Option<
    unsafe extern "C" fn(
        ah: *mut sys::switch_asr_handle_t,
        name: *const c_char,
    ) -> sys::switch_status_t,
>;
pub(crate) type AsrCloseFn = Option<
    unsafe extern "C" fn(
        ah: *mut sys::switch_asr_handle_t,
        flags: *mut sys::switch_asr_flag_t,
    ) -> sys::switch_status_t,
>;
pub(crate) type AsrFeedFn = Option<
    unsafe extern "C" fn(
        ah: *mut sys::switch_asr_handle_t,
        data: *mut std::ffi::c_void,
        len: std::os::raw::c_uint,
        flags: *mut sys::switch_asr_flag_t,
    ) -> sys::switch_status_t,
>;
pub(crate) type DirectoryOpenFn = Option<
    unsafe extern "C" fn(
        dh: *mut sys::switch_directory_handle_t,
        source: *mut c_char,
        dsn: *mut c_char,
        passwd: *mut c_char,
    ) -> sys::switch_status_t,
>;
pub(crate) type DirectoryCloseFn =
    Option<unsafe extern "C" fn(dh: *mut sys::switch_directory_handle_t) -> sys::switch_status_t>;
pub(crate) type DirectoryQueryFn = Option<
    unsafe extern "C" fn(
        dh: *mut sys::switch_directory_handle_t,
        base: *mut c_char,
        query: *mut c_char,
    ) -> sys::switch_status_t,
>;
pub(crate) type DirectoryNextFn =
    Option<unsafe extern "C" fn(dh: *mut sys::switch_directory_handle_t) -> sys::switch_status_t>;
pub(crate) type DirectoryNextPairFn = Option<
    unsafe extern "C" fn(
        dh: *mut sys::switch_directory_handle_t,
        var: *mut *mut c_char,
        val: *mut *mut c_char,
    ) -> sys::switch_status_t,
>;
pub(crate) type DbHandleNewFn = Option<
    unsafe extern "C" fn(
        database_interface_options: sys::switch_cache_db_database_interface_options_t,
        dih: *mut *mut sys::switch_database_interface_handle_t,
    ) -> sys::switch_status_t,
>;
pub(crate) type DbHandleDestroyFn = Option<
    unsafe extern "C" fn(
        dih: *mut *mut sys::switch_database_interface_handle_t,
    ) -> sys::switch_status_t,
>;
pub(crate) type DbExecDetailedFn = Option<
    unsafe extern "C" fn(
        file: *const c_char,
        func: *const c_char,
        line: std::os::raw::c_int,
        dih: *mut sys::switch_database_interface_handle_t,
        sql: *const c_char,
        err: *mut *mut c_char,
    ) -> sys::switch_status_t,
>;

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
    pub(crate) unsafe fn create(
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

    pub(crate) fn as_ptr(&self) -> *mut sys::switch_loadable_module_interface_t {
        self.raw.as_ptr()
    }

    /// Registers a FreeSWITCH API command on this module.
    #[allow(clippy::missing_transmute_annotations)]
    pub fn add_api(
        self,
        name: impl StaticCStr,
        description: impl StaticCStr,
        syntax: impl StaticCStr,
        function: unsafe extern "C" fn(
            *const c_char,
            *mut std::ffi::c_void,
            *mut std::ffi::c_void,
        ) -> crate::Status,
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
            // SAFETY: `function` is ABI-identical to the field's `switch_api_function_t`:
            // pointer params differ only in pointee type, and `Status` is `#[repr(transparent)]`
            // over `switch_status_t`. The transmute is a sound bitcast.
            (*api_ref).function = std::mem::transmute(Some(function));
            (*api_ref).syntax = syntax.as_ptr();
        }

        Ok(ApiInterface { raw: api })
    }

    #[allow(clippy::missing_transmute_annotations)]
    pub fn add_application(
        self,
        info: ApplicationInfo,
        function: unsafe extern "C" fn(*mut std::ffi::c_void, *const c_char),
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
            // SAFETY: `function` is ABI-identical to `switch_application_function_t` — the
            // pointer param differs only in pointee type. Sound bitcast.
            (*application_ref).application_function = std::mem::transmute(Some(function));
            (*application_ref).long_desc = strings.long_description.as_ptr();
            (*application_ref).short_desc = strings.short_description.as_ptr();
            (*application_ref).syntax = strings.syntax.as_ptr();
        }

        Ok(ApplicationInterface { raw: application })
    }

    #[allow(clippy::missing_transmute_annotations)]
    pub fn add_chat_application(
        self,
        info: ApplicationInfo,
        function: unsafe extern "C" fn(*mut std::ffi::c_void, *const c_char) -> crate::Status,
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
            // SAFETY: `function` is ABI-identical to `switch_chat_application_function_t` —
            // pointer param differs only in pointee type, and `Status` is `#[repr(transparent)]`
            // over `switch_status_t`. Sound bitcast.
            (*application_ref).chat_application_function = std::mem::transmute(Some(function));
            (*application_ref).long_desc = strings.long_description.as_ptr();
            (*application_ref).short_desc = strings.short_description.as_ptr();
            (*application_ref).syntax = strings.syntax.as_ptr();
        }

        Ok(ChatApplicationInterface { raw: application })
    }

    pub(crate) fn add_endpoint(
        self,
        name: impl StaticCStr,
        io_routines: *mut sys::switch_io_routines_t,
        state_handler: *mut sys::switch_state_handler_table_t,
    ) -> Result<EndpointInterface> {
        let name = name.into_static_cstr()?;
        let endpoint = create_interface::<sys::switch_endpoint_interface_t>(
            self.raw,
            sys::switch_module_interface_name_t::SWITCH_ENDPOINT_INTERFACE,
        )?;

        // SAFETY: `endpoint` is a valid interface allocation returned by FreeSWITCH. `name` has a
        // static lifetime, and the caller supplies module-owned I/O routine storage and state
        // handler table. FreeSWITCH's state machine (`switch_core_session_run`) asserts
        // `state_handler != NULL`, so the caller MUST provide a non-null table (all-NULL
        // callbacks are valid — each `on_*` NULL entry is treated as "no-op success").
        unsafe {
            let endpoint_ref = endpoint.as_ptr();
            (*endpoint_ref).interface_name = name.as_ptr();
            (*endpoint_ref).io_routines = io_routines;
            (*endpoint_ref).state_handler = state_handler;
        }

        Ok(EndpointInterface { raw: endpoint })
    }

    /// Registers a dialplan interface — a `hunt` callback that routes calls.
    pub(crate) fn add_dialplan(
        self,
        name: impl StaticCStr,
        hunt: sys::switch_dialplan_hunt_function_t,
    ) -> Result<DialplanInterface> {
        let name = name.into_static_cstr()?;
        let iface = create_interface::<sys::switch_dialplan_interface>(
            self.raw,
            sys::switch_module_interface_name_t::SWITCH_DIALPLAN_INTERFACE,
        )?;
        // SAFETY: `iface` is a valid interface allocation; `name` is static. Only set the
        // user-owned fields (interface_name, hunt_function); rwlock/refs/reflock/parent/next are
        // FreeSWITCH-owned runtime bookkeeping left zeroed by create_interface.
        unsafe {
            let r = iface.as_ptr();
            (*r).interface_name = name.as_ptr();
            (*r).hunt_function = hunt;
        }
        Ok(DialplanInterface { raw: iface })
    }

    /// Registers a timer interface — a set of `*mut switch_timer_t` callbacks.
    #[allow(clippy::too_many_arguments)]
    pub fn add_timer(
        self,
        name: impl StaticCStr,
        timer_init: TimerInitFn,
        timer_next: TimerNextFn,
        timer_step: TimerStepFn,
        timer_sync: TimerSyncFn,
        timer_check: TimerCheckFn,
        timer_destroy: TimerDestroyFn,
    ) -> Result<TimerInterface> {
        let name = name.into_static_cstr()?;
        let iface = create_interface::<sys::switch_timer_interface>(
            self.raw,
            sys::switch_module_interface_name_t::SWITCH_TIMER_INTERFACE,
        )?;
        unsafe {
            let r = iface.as_ptr();
            (*r).interface_name = name.as_ptr();
            (*r).timer_init = timer_init;
            (*r).timer_next = timer_next;
            (*r).timer_step = timer_step;
            (*r).timer_sync = timer_sync;
            (*r).timer_check = timer_check;
            (*r).timer_destroy = timer_destroy;
        }
        Ok(TimerInterface { raw: iface })
    }

    /// Registers a file-format interface — read/write/seek callbacks over `*mut switch_file_handle_t`.
    pub fn add_file(
        self,
        name: impl StaticCStr,
        file_open: FileOpenFn,
        file_close: FileCloseFn,
        file_truncate: FileTruncateFn,
        file_read: FileReadFn,
        file_write: FileWriteFn,
    ) -> Result<FileInterface> {
        let name = name.into_static_cstr()?;
        let iface = create_interface::<sys::switch_file_interface>(
            self.raw,
            sys::switch_module_interface_name_t::SWITCH_FILE_INTERFACE,
        )?;
        unsafe {
            let r = iface.as_ptr();
            (*r).interface_name = name.as_ptr();
            (*r).file_open = file_open;
            (*r).file_close = file_close;
            (*r).file_truncate = file_truncate;
            (*r).file_read = file_read;
            (*r).file_write = file_write;
        }
        Ok(FileInterface { raw: iface })
    }

    /// Registers a speech (TTS) interface — callbacks over `*mut switch_speech_handle_t`.
    pub fn add_speech(
        self,
        name: impl StaticCStr,
        speech_open: SpeechOpenFn,
        speech_close: SpeechCloseFn,
        speech_feed_tts: SpeechFeedTtsFn,
        speech_read_tts: SpeechReadTtsFn,
    ) -> Result<SpeechInterface> {
        let name = name.into_static_cstr()?;
        let iface = create_interface::<sys::switch_speech_interface>(
            self.raw,
            sys::switch_module_interface_name_t::SWITCH_SPEECH_INTERFACE,
        )?;
        unsafe {
            let r = iface.as_ptr();
            (*r).interface_name = name.as_ptr();
            (*r).speech_open = speech_open;
            (*r).speech_close = speech_close;
            (*r).speech_feed_tts = speech_feed_tts;
            (*r).speech_read_tts = speech_read_tts;
        }
        Ok(SpeechInterface { raw: iface })
    }

    /// Registers an ASR interface — callbacks over `*mut switch_asr_handle_t`.
    pub fn add_asr(
        self,
        name: impl StaticCStr,
        asr_open: AsrOpenFn,
        asr_load_grammar: AsrLoadGrammarFn,
        asr_unload_grammar: AsrUnloadGrammarFn,
        asr_close: AsrCloseFn,
        asr_feed: AsrFeedFn,
    ) -> Result<AsrInterface> {
        let name = name.into_static_cstr()?;
        let iface = create_interface::<sys::switch_asr_interface>(
            self.raw,
            sys::switch_module_interface_name_t::SWITCH_ASR_INTERFACE,
        )?;
        unsafe {
            let r = iface.as_ptr();
            (*r).interface_name = name.as_ptr();
            (*r).asr_open = asr_open;
            (*r).asr_load_grammar = asr_load_grammar;
            (*r).asr_unload_grammar = asr_unload_grammar;
            (*r).asr_close = asr_close;
            (*r).asr_feed = asr_feed;
        }
        Ok(AsrInterface { raw: iface })
    }

    /// Registers a `say` interface (number/date/time pronunciation).
    pub(crate) fn add_say(
        self,
        name: impl StaticCStr,
        say_function: sys::switch_say_callback_t,
        say_string_function: sys::switch_say_string_callback_t,
    ) -> Result<SayInterface> {
        let name = name.into_static_cstr()?;
        let iface = create_interface::<sys::switch_say_interface>(
            self.raw,
            sys::switch_module_interface_name_t::SWITCH_SAY_INTERFACE,
        )?;
        unsafe {
            let r = iface.as_ptr();
            (*r).interface_name = name.as_ptr();
            (*r).say_function = say_function;
            (*r).say_string_function = say_string_function;
        }
        Ok(SayInterface { raw: iface })
    }

    /// Registers a directory interface — LDAP-style lookups over `*mut switch_directory_handle_t`.
    pub fn add_directory(
        self,
        name: impl StaticCStr,
        directory_open: DirectoryOpenFn,
        directory_close: DirectoryCloseFn,
        directory_query: DirectoryQueryFn,
        directory_next: DirectoryNextFn,
        directory_next_pair: DirectoryNextPairFn,
    ) -> Result<DirectoryInterface> {
        let name = name.into_static_cstr()?;
        let iface = create_interface::<sys::switch_directory_interface>(
            self.raw,
            sys::switch_module_interface_name_t::SWITCH_DIRECTORY_INTERFACE,
        )?;
        unsafe {
            let r = iface.as_ptr();
            (*r).interface_name = name.as_ptr();
            (*r).directory_open = directory_open;
            (*r).directory_close = directory_close;
            (*r).directory_query = directory_query;
            (*r).directory_next = directory_next;
            (*r).directory_next_pair = directory_next_pair;
        }
        Ok(DirectoryInterface { raw: iface })
    }

    /// Registers a chat transport interface — an outbound `chat_send` callback.
    pub fn add_chat(self, name: impl StaticCStr, chat_send: ChatSendFn) -> Result<ChatInterface> {
        let name = name.into_static_cstr()?;
        let iface = create_interface::<sys::switch_chat_interface>(
            self.raw,
            sys::switch_module_interface_name_t::SWITCH_CHAT_INTERFACE,
        )?;
        unsafe {
            let r = iface.as_ptr();
            (*r).interface_name = name.as_ptr();
            (*r).chat_send = chat_send;
        }
        Ok(ChatInterface { raw: iface })
    }

    /// Registers a management interface — an SNMP-style `management_function` keyed by
    /// `relative_oid`. Note: the identifier field is `relative_oid`, not `interface_name`.
    pub fn add_management(
        self,
        relative_oid: impl StaticCStr,
        management_function: ManagementFn,
    ) -> Result<ManagementInterface> {
        let relative_oid = relative_oid.into_static_cstr()?;
        let iface = create_interface::<sys::switch_management_interface>(
            self.raw,
            sys::switch_module_interface_name_t::SWITCH_MANAGEMENT_INTERFACE,
        )?;
        unsafe {
            let r = iface.as_ptr();
            (*r).relative_oid = relative_oid.as_ptr();
            (*r).management_function = management_function;
        }
        Ok(ManagementInterface { raw: iface })
    }

    /// Registers a limit interface — a call-cap backend with incr/release/usage/reset/etc.
    #[allow(clippy::too_many_arguments)]
    pub fn add_limit(
        self,
        name: impl StaticCStr,
        incr: LimitIncrFn,
        release: LimitReleaseFn,
        usage: LimitUsageFn,
        reset: LimitResetFn,
        status: LimitStatusFn,
        interval_reset: LimitIntervalResetFn,
    ) -> Result<LimitInterface> {
        let name = name.into_static_cstr()?;
        let iface = create_interface::<sys::switch_limit_interface>(
            self.raw,
            sys::switch_module_interface_name_t::SWITCH_LIMIT_INTERFACE,
        )?;
        unsafe {
            let r = iface.as_ptr();
            (*r).interface_name = name.as_ptr();
            (*r).incr = incr;
            (*r).release = release;
            (*r).usage = usage;
            (*r).reset = reset;
            (*r).status = status;
            (*r).interval_reset = interval_reset;
        }
        Ok(LimitInterface { raw: iface })
    }

    /// Registers a JSON API command — like [`Module::add_api`] but returning a cJSON object.
    pub(crate) fn add_json_api(
        self,
        name: impl StaticCStr,
        description: impl StaticCStr,
        syntax: impl StaticCStr,
        function: sys::switch_json_api_function_t,
    ) -> Result<JsonApiInterface> {
        let name = name.into_static_cstr()?;
        let description = description.into_static_cstr()?;
        let syntax = syntax.into_static_cstr()?;
        let iface = create_interface::<sys::switch_json_api_interface>(
            self.raw,
            sys::switch_module_interface_name_t::SWITCH_JSON_API_INTERFACE,
        )?;
        unsafe {
            let r = iface.as_ptr();
            (*r).interface_name = name.as_ptr();
            (*r).desc = description.as_ptr();
            (*r).function = function;
            (*r).syntax = syntax.as_ptr();
        }
        Ok(JsonApiInterface { raw: iface })
    }

    /// Registers a pluggable database backend. The `handle_new`/`handle_destroy` callbacks manage
    /// `switch_database_interface_handle_t` storage; the rest operate on such a handle.
    pub fn add_database(
        self,
        name: impl StaticCStr,
        handle_new: DbHandleNewFn,
        handle_destroy: DbHandleDestroyFn,
        exec_detailed: DbExecDetailedFn,
    ) -> Result<DatabaseInterface> {
        let name = name.into_static_cstr()?;
        let iface = create_interface::<sys::switch_database_interface>(
            self.raw,
            sys::switch_module_interface_name_t::SWITCH_DATABASE_INTERFACE,
        )?;
        unsafe {
            let r = iface.as_ptr();
            (*r).interface_name = name.as_ptr();
            (*r).handle_new = handle_new;
            (*r).handle_destroy = handle_destroy;
            (*r).exec_detailed = exec_detailed;
        }
        Ok(DatabaseInterface { raw: iface })
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
    /// `slot`/`pool` are the raw `*mut c_void` pointers the `module_load!` trampoline receives
    /// (pointee-erased so the macro never names a `sys` type); they are cast back to the real
    /// FreeSWITCH pointer types here before reaching `Module::create`.
    ///
    /// # Safety
    ///
    /// `slot` and `pool` must be the live loader-owned pointers passed by FreeSWITCH to this
    /// module's load callback. `slot` must be writable for one module interface pointer.
    pub unsafe fn new(
        slot: *mut *mut std::ffi::c_void,
        pool: *mut std::ffi::c_void,
        name: impl StaticCStr,
    ) -> Result<Self> {
        Ok(Self {
            // SAFETY: Forwarded from `ModuleBuilder::new`'s caller; the `c_void` pointers are
            // the real FreeSWITCH loader pointers, just pointee-erased.
            module: unsafe { Module::create(slot.cast(), pool.cast(), name)? },
        })
    }

    pub fn api(
        self,
        name: impl StaticCStr,
        description: impl StaticCStr,
        syntax: impl StaticCStr,
        function: unsafe extern "C" fn(
            *const c_char,
            *mut std::ffi::c_void,
            *mut std::ffi::c_void,
        ) -> crate::Status,
    ) -> Result<Self> {
        self.module.add_api(name, description, syntax, function)?;
        Ok(self)
    }

    pub fn application(
        self,
        info: ApplicationInfo,
        function: unsafe extern "C" fn(*mut std::ffi::c_void, *const c_char),
    ) -> Result<Self> {
        self.module.add_application(info, function)?;
        Ok(self)
    }

    pub fn chat_application(
        self,
        info: ApplicationInfo,
        function: unsafe extern "C" fn(*mut std::ffi::c_void, *const c_char) -> crate::Status,
    ) -> Result<Self> {
        self.module.add_chat_application(info, function)?;
        Ok(self)
    }

    pub fn endpoint(
        self,
        name: impl StaticCStr,
        io_routines: crate::endpoint::IoRoutines,
        state_handler: crate::endpoint::StateHandlerTable,
    ) -> Result<Self> {
        self.module
            .add_endpoint(name, io_routines.as_ptr(), state_handler.as_ptr())?;
        Ok(self)
    }

    pub fn finish(self) -> Module {
        self.module
    }
}

/// `#[doc(hidden)]` transparent wrapper over FreeSWITCH's
/// `switch_loadable_module_function_table_t`.
///
/// Exists so the [`macro@module_exports`] macro can declare a module-interface table in a
/// downstream crate using a `fswtch`-owned type name instead of a raw `*-sys` type. Construct it
/// only through [`__ModuleFunctionTable::__new`]; it is an internal FFI detail, not part of the
/// public API.
#[repr(transparent)]
#[doc(hidden)]
pub struct __ModuleFunctionTable(pub(crate) sys::switch_loadable_module_function_table_t);

impl __ModuleFunctionTable {
    /// Builds a module-interface table from `Status`-returning / `c_void`-typed callbacks.
    ///
    /// `Status` is `#[repr(transparent)]` over `switch_status_t`, and pointer params differ from
    /// the C function-pointer field types only in pointee type (which does not affect the C
    /// calling convention). Each callback is therefore bit-for-bit ABI-compatible with the
    /// corresponding FreeSWITCH `switch_status_t`-returning C function-pointer field, and the
    /// conversion is a `transmute` (a no-op bitcast).
    ///
    /// # Safety
    ///
    /// `load`/`shutdown`/`runtime` must be `extern "C" fn` / `unsafe extern "C" fn` pointers
    /// whose ABI matches the FreeSWITCH function-pointer field types. The [`macro@module_exports`]
    /// macro supplies exactly these (the `module_load!` trampoline for `load`, and user
    /// `-> fswtch::Status` fns for `shutdown`/`runtime`); do not call `__new` directly.
    #[doc(hidden)]
    #[allow(clippy::missing_transmute_annotations)]
    pub const unsafe fn __new(
        load: Option<
            unsafe extern "C" fn(
                *mut *mut std::ffi::c_void,
                *mut std::ffi::c_void,
            ) -> crate::Status,
        >,
        shutdown: Option<extern "C" fn() -> crate::Status>,
        runtime: Option<extern "C" fn() -> crate::Status>,
    ) -> Self {
        // SAFETY: `Status` is `#[repr(transparent)]` over `switch_status_t`; `c_void` pointees
        // are ABI-identical to the real FreeSWITCH pointer types. All three fn-pointer options
        // are the same size as their `sys` field counterparts, so this is a sound bitcast.
        Self(sys::switch_loadable_module_function_table_t {
            switch_api_version: sys::SWITCH_API_VERSION as _,
            load: unsafe { std::mem::transmute(load) },
            shutdown: unsafe { std::mem::transmute(shutdown) },
            runtime: unsafe { std::mem::transmute(runtime) },
            flags: 0,
        })
    }
}

#[derive(Copy, Clone)]
pub struct ApiInterface {
    raw: NonNull<sys::switch_api_interface_t>,
}

impl ApiInterface {
    pub(crate) fn as_ptr(&self) -> *mut sys::switch_api_interface_t {
        self.raw.as_ptr()
    }
}

#[derive(Copy, Clone)]
pub struct ApplicationInterface {
    raw: NonNull<sys::switch_application_interface_t>,
}

impl ApplicationInterface {
    pub(crate) fn as_ptr(&self) -> *mut sys::switch_application_interface_t {
        self.raw.as_ptr()
    }
}

#[derive(Copy, Clone)]
pub struct ChatApplicationInterface {
    raw: NonNull<sys::switch_chat_application_interface_t>,
}

impl ChatApplicationInterface {
    pub(crate) fn as_ptr(&self) -> *mut sys::switch_chat_application_interface_t {
        self.raw.as_ptr()
    }
}

#[derive(Copy, Clone)]
pub struct EndpointInterface {
    raw: NonNull<sys::switch_endpoint_interface_t>,
}

impl EndpointInterface {
    pub(crate) fn as_ptr(&self) -> *mut sys::switch_endpoint_interface_t {
        self.raw.as_ptr()
    }
}

#[derive(Copy, Clone)]
pub struct DialplanInterface {
    raw: NonNull<sys::switch_dialplan_interface>,
}

impl DialplanInterface {
    pub(crate) fn as_ptr(&self) -> *mut sys::switch_dialplan_interface {
        self.raw.as_ptr()
    }
}

#[derive(Copy, Clone)]
pub struct TimerInterface {
    raw: NonNull<sys::switch_timer_interface>,
}

impl TimerInterface {
    pub(crate) fn as_ptr(&self) -> *mut sys::switch_timer_interface {
        self.raw.as_ptr()
    }
}

#[derive(Copy, Clone)]
pub struct FileInterface {
    raw: NonNull<sys::switch_file_interface>,
}

impl FileInterface {
    pub(crate) fn as_ptr(&self) -> *mut sys::switch_file_interface {
        self.raw.as_ptr()
    }
}

#[derive(Copy, Clone)]
pub struct SpeechInterface {
    raw: NonNull<sys::switch_speech_interface>,
}

impl SpeechInterface {
    pub(crate) fn as_ptr(&self) -> *mut sys::switch_speech_interface {
        self.raw.as_ptr()
    }
}

#[derive(Copy, Clone)]
pub struct AsrInterface {
    raw: NonNull<sys::switch_asr_interface>,
}

impl AsrInterface {
    pub(crate) fn as_ptr(&self) -> *mut sys::switch_asr_interface {
        self.raw.as_ptr()
    }
}

#[derive(Copy, Clone)]
pub struct SayInterface {
    raw: NonNull<sys::switch_say_interface>,
}

impl SayInterface {
    pub(crate) fn as_ptr(&self) -> *mut sys::switch_say_interface {
        self.raw.as_ptr()
    }
}

#[derive(Copy, Clone)]
pub struct DirectoryInterface {
    raw: NonNull<sys::switch_directory_interface>,
}

impl DirectoryInterface {
    pub(crate) fn as_ptr(&self) -> *mut sys::switch_directory_interface {
        self.raw.as_ptr()
    }
}

#[derive(Copy, Clone)]
pub struct ChatInterface {
    raw: NonNull<sys::switch_chat_interface>,
}

impl ChatInterface {
    pub(crate) fn as_ptr(&self) -> *mut sys::switch_chat_interface {
        self.raw.as_ptr()
    }
}

#[derive(Copy, Clone)]
pub struct ManagementInterface {
    raw: NonNull<sys::switch_management_interface>,
}

impl ManagementInterface {
    pub(crate) fn as_ptr(&self) -> *mut sys::switch_management_interface {
        self.raw.as_ptr()
    }
}

#[derive(Copy, Clone)]
pub struct LimitInterface {
    raw: NonNull<sys::switch_limit_interface>,
}

impl LimitInterface {
    pub(crate) fn as_ptr(&self) -> *mut sys::switch_limit_interface {
        self.raw.as_ptr()
    }
}

#[derive(Copy, Clone)]
pub struct JsonApiInterface {
    raw: NonNull<sys::switch_json_api_interface>,
}

impl JsonApiInterface {
    pub(crate) fn as_ptr(&self) -> *mut sys::switch_json_api_interface {
        self.raw.as_ptr()
    }
}

#[derive(Copy, Clone)]
pub struct DatabaseInterface {
    raw: NonNull<sys::switch_database_interface>,
}

impl DatabaseInterface {
    pub(crate) fn as_ptr(&self) -> *mut sys::switch_database_interface {
        self.raw.as_ptr()
    }
}

/// Compile-time assertion that every callback alias matches the corresponding bindgen field type.
/// If a future bindgen regen changes a field signature, this fails to compile at the offending line.
#[cfg(test)]
mod alias_type_checks {
    use super::*;

    #[test]
    fn timer_aliases_match_fields() {
        let r: sys::switch_timer_interface = Default::default();
        let _: TimerInitFn = r.timer_init;
        let _: TimerNextFn = r.timer_next;
        let _: TimerStepFn = r.timer_step;
        let _: TimerSyncFn = r.timer_sync;
        let _: TimerCheckFn = r.timer_check;
        let _: TimerDestroyFn = r.timer_destroy;
    }

    #[test]
    fn file_aliases_match_fields() {
        let r: sys::switch_file_interface = Default::default();
        let _: FileOpenFn = r.file_open;
        let _: FileCloseFn = r.file_close;
        let _: FileTruncateFn = r.file_truncate;
        let _: FileReadFn = r.file_read;
        let _: FileWriteFn = r.file_write;
    }

    #[test]
    fn speech_aliases_match_fields() {
        let r: sys::switch_speech_interface = Default::default();
        let _: SpeechOpenFn = r.speech_open;
        let _: SpeechCloseFn = r.speech_close;
        let _: SpeechFeedTtsFn = r.speech_feed_tts;
        let _: SpeechReadTtsFn = r.speech_read_tts;
    }

    #[test]
    fn asr_aliases_match_fields() {
        let r: sys::switch_asr_interface = Default::default();
        let _: AsrOpenFn = r.asr_open;
        let _: AsrLoadGrammarFn = r.asr_load_grammar;
        let _: AsrUnloadGrammarFn = r.asr_unload_grammar;
        let _: AsrCloseFn = r.asr_close;
        let _: AsrFeedFn = r.asr_feed;
    }

    #[test]
    fn directory_aliases_match_fields() {
        let r: sys::switch_directory_interface = Default::default();
        let _: DirectoryOpenFn = r.directory_open;
        let _: DirectoryCloseFn = r.directory_close;
        let _: DirectoryQueryFn = r.directory_query;
        let _: DirectoryNextFn = r.directory_next;
        let _: DirectoryNextPairFn = r.directory_next_pair;
    }

    #[test]
    fn chat_management_aliases_match_fields() {
        let chat: sys::switch_chat_interface = Default::default();
        let _: ChatSendFn = chat.chat_send;
        let mgmt: sys::switch_management_interface = Default::default();
        let _: ManagementFn = mgmt.management_function;
    }

    #[test]
    fn limit_aliases_match_fields() {
        let r: sys::switch_limit_interface = Default::default();
        let _: LimitIncrFn = r.incr;
        let _: LimitReleaseFn = r.release;
        let _: LimitUsageFn = r.usage;
        let _: LimitResetFn = r.reset;
        let _: LimitStatusFn = r.status;
        let _: LimitIntervalResetFn = r.interval_reset;
    }

    #[test]
    fn database_aliases_match_fields() {
        let r: sys::switch_database_interface = Default::default();
        let _: DbHandleNewFn = r.handle_new;
        let _: DbHandleDestroyFn = r.handle_destroy;
        let _: DbExecDetailedFn = r.exec_detailed;
    }

    /// Load-bearing ABI invariant for every callback `transmute` in this crate: the public
    /// `extern "C" fn(...) -> Status` callback types are bitcast onto FreeSWITCH's
    /// `... -> switch_status_t` field types. That bitcast is sound only while `Status` stays
    /// `#[repr(transparent)]` over `sys::switch_status_t`. (Function-pointer *arity* drift cannot
    /// be compile-checked — the source and target types intentionally differ, which is exactly why
    /// `transmute` is required — so this size/align assertion is the strongest static guard
    /// available for the eight transmute sites in `module.rs` / `event.rs`.)
    #[test]
    fn status_is_abi_compatible_with_switch_status_t() {
        use std::mem::{align_of, size_of};
        assert_eq!(
            size_of::<crate::Status>(),
            size_of::<sys::switch_status_t>()
        );
        assert_eq!(
            align_of::<crate::Status>(),
            align_of::<sys::switch_status_t>()
        );
    }
}
