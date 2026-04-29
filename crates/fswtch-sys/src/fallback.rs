use std::os::raw::{c_char, c_int, c_uint, c_void};

pub type switch_size_t = usize;
pub type switch_bool_t = c_uint;
pub type switch_module_flag_t = u32;

pub const SWITCH_FALSE: switch_bool_t = 0;
pub const SWITCH_TRUE: switch_bool_t = 1;
pub const SWITCH_API_VERSION: c_int = 5;

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum switch_status_t {
    SWITCH_STATUS_SUCCESS = 0,
    SWITCH_STATUS_FALSE = 1,
    SWITCH_STATUS_TIMEOUT = 2,
    SWITCH_STATUS_RESTART = 3,
    SWITCH_STATUS_INTR = 4,
    SWITCH_STATUS_NOTIMPL = 5,
    SWITCH_STATUS_MEMERR = 6,
    SWITCH_STATUS_NOOP = 7,
    SWITCH_STATUS_RESAMPLE = 8,
    SWITCH_STATUS_GENERR = 9,
    SWITCH_STATUS_INUSE = 10,
    SWITCH_STATUS_BREAK = 11,
    SWITCH_STATUS_SOCKERR = 12,
    SWITCH_STATUS_MORE_DATA = 13,
    SWITCH_STATUS_NOTFOUND = 14,
    SWITCH_STATUS_UNLOAD = 15,
    SWITCH_STATUS_NOUNLOAD = 16,
    SWITCH_STATUS_IGNORE = 17,
    SWITCH_STATUS_TOO_SMALL = 18,
    SWITCH_STATUS_FOUND = 19,
    SWITCH_STATUS_CONTINUE = 20,
    SWITCH_STATUS_TERM = 21,
    SWITCH_STATUS_NOT_INITALIZED = 22,
    SWITCH_STATUS_TOO_LATE = 23,
    SWITCH_STATUS_XBREAK = 35,
    SWITCH_STATUS_WINBREAK = 730035,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum switch_module_interface_name_t {
    SWITCH_ENDPOINT_INTERFACE = 0,
    SWITCH_TIMER_INTERFACE = 1,
    SWITCH_DIALPLAN_INTERFACE = 2,
    SWITCH_CODEC_INTERFACE = 3,
    SWITCH_APPLICATION_INTERFACE = 4,
    SWITCH_API_INTERFACE = 5,
    SWITCH_FILE_INTERFACE = 6,
    SWITCH_SPEECH_INTERFACE = 7,
    SWITCH_DIRECTORY_INTERFACE = 8,
    SWITCH_CHAT_INTERFACE = 9,
    SWITCH_SAY_INTERFACE = 10,
    SWITCH_ASR_INTERFACE = 11,
    SWITCH_MANAGEMENT_INTERFACE = 12,
    SWITCH_LIMIT_INTERFACE = 13,
    SWITCH_CHAT_APPLICATION_INTERFACE = 14,
    SWITCH_JSON_API_INTERFACE = 15,
    SWITCH_DATABASE_INTERFACE = 16,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum switch_stack_t {
    SWITCH_STACK_BOTTOM = 1,
    SWITCH_STACK_TOP = 2,
    SWITCH_STACK_UNSHIFT = 4,
    SWITCH_STACK_PUSH = 8,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum switch_event_types_t {
    SWITCH_EVENT_CUSTOM = 0,
    SWITCH_EVENT_CLONE = 1,
    SWITCH_EVENT_CHANNEL_CREATE = 2,
    SWITCH_EVENT_CHANNEL_DESTROY = 3,
    SWITCH_EVENT_CHANNEL_STATE = 4,
    SWITCH_EVENT_CHANNEL_CALLSTATE = 5,
    SWITCH_EVENT_CHANNEL_ANSWER = 6,
    SWITCH_EVENT_CHANNEL_HANGUP = 7,
    SWITCH_EVENT_CHANNEL_HANGUP_COMPLETE = 8,
    SWITCH_EVENT_CHANNEL_EXECUTE = 9,
    SWITCH_EVENT_CHANNEL_EXECUTE_COMPLETE = 10,
}

#[repr(C)]
pub struct switch_memory_pool_t {
    _private: [u8; 0],
}

#[repr(C)]
pub struct switch_core_session_t {
    _private: [u8; 0],
}

#[repr(C)]
pub struct switch_event_t {
    _private: [u8; 0],
}

#[repr(C)]
pub struct switch_thread_rwlock_t {
    _private: [u8; 0],
}

#[repr(C)]
pub struct switch_mutex_t {
    _private: [u8; 0],
}

pub type switch_stream_handle_read_function_t =
    Option<unsafe extern "C" fn(handle: *mut switch_stream_handle_t, len: *mut c_int) -> *mut u8>;
pub type switch_stream_handle_write_function_t = Option<
    unsafe extern "C" fn(
        handle: *mut switch_stream_handle_t,
        fmt: *const c_char,
        ...
    ) -> switch_status_t,
>;
pub type switch_stream_handle_raw_write_function_t = Option<
    unsafe extern "C" fn(
        handle: *mut switch_stream_handle_t,
        data: *mut u8,
        datalen: switch_size_t,
    ) -> switch_status_t,
>;

#[repr(C)]
pub struct switch_stream_handle_t {
    pub read_function: switch_stream_handle_read_function_t,
    pub write_function: switch_stream_handle_write_function_t,
    pub raw_write_function: switch_stream_handle_raw_write_function_t,
    pub data: *mut c_void,
    pub end: *mut c_void,
    pub data_size: switch_size_t,
    pub data_len: switch_size_t,
    pub alloc_len: switch_size_t,
    pub alloc_chunk: switch_size_t,
    pub param_event: *mut switch_event_t,
}

pub type switch_api_function_t = Option<
    unsafe extern "C" fn(
        cmd: *const c_char,
        session: *mut switch_core_session_t,
        stream: *mut switch_stream_handle_t,
    ) -> switch_status_t,
>;

pub type switch_module_load_t = Option<
    unsafe extern "C" fn(
        module_interface: *mut *mut switch_loadable_module_interface_t,
        pool: *mut switch_memory_pool_t,
    ) -> switch_status_t,
>;
pub type switch_module_runtime_t = Option<unsafe extern "C" fn() -> switch_status_t>;
pub type switch_module_shutdown_t = Option<unsafe extern "C" fn() -> switch_status_t>;

#[repr(C)]
pub struct switch_loadable_module_function_table_t {
    pub switch_api_version: c_int,
    pub load: switch_module_load_t,
    pub shutdown: switch_module_shutdown_t,
    pub runtime: switch_module_runtime_t,
    pub flags: switch_module_flag_t,
}

#[repr(C)]
pub struct switch_loadable_module_interface_t {
    pub module_name: *const c_char,
    pub endpoint_interface: *mut c_void,
    pub timer_interface: *mut c_void,
    pub dialplan_interface: *mut c_void,
    pub codec_interface: *mut c_void,
    pub application_interface: *mut c_void,
    pub chat_application_interface: *mut c_void,
    pub api_interface: *mut switch_api_interface_t,
    pub json_api_interface: *mut c_void,
    pub file_interface: *mut c_void,
    pub speech_interface: *mut c_void,
    pub directory_interface: *mut c_void,
    pub chat_interface: *mut c_void,
    pub say_interface: *mut c_void,
    pub asr_interface: *mut c_void,
    pub management_interface: *mut c_void,
    pub limit_interface: *mut c_void,
    pub database_interface: *mut c_void,
    pub rwlock: *mut switch_thread_rwlock_t,
    pub refs: c_int,
    pub pool: *mut switch_memory_pool_t,
}

#[repr(C)]
pub struct switch_api_interface_t {
    pub interface_name: *const c_char,
    pub desc: *const c_char,
    pub function: switch_api_function_t,
    pub syntax: *const c_char,
    pub rwlock: *mut switch_thread_rwlock_t,
    pub refs: c_int,
    pub reflock: *mut switch_mutex_t,
    pub parent: *mut switch_loadable_module_interface_t,
    pub next: *mut switch_api_interface_t,
}

unsafe extern "C" {
    pub fn switch_loadable_module_create_module_interface(
        pool: *mut switch_memory_pool_t,
        name: *const c_char,
    ) -> *mut switch_loadable_module_interface_t;

    pub fn switch_loadable_module_create_interface(
        module: *mut switch_loadable_module_interface_t,
        iname: switch_module_interface_name_t,
    ) -> *mut c_void;

    pub fn switch_event_create_subclass_detailed(
        file: *const c_char,
        func: *const c_char,
        line: c_int,
        event: *mut *mut switch_event_t,
        event_id: switch_event_types_t,
        subclass_name: *const c_char,
    ) -> switch_status_t;

    pub fn switch_event_add_header_string(
        event: *mut switch_event_t,
        stack: switch_stack_t,
        header_name: *const c_char,
        data: *const c_char,
    ) -> switch_status_t;

    pub fn switch_event_fire_detailed(
        file: *const c_char,
        func: *const c_char,
        line: c_int,
        event: *mut *mut switch_event_t,
        user_data: *mut c_void,
    ) -> switch_status_t;
}
