//! Raw FFI bindings over the vendored WebRTC AEC3 C++.
//!
//! The C++ is built from the vendored tree under `cpp/` by `build.rs` (CMake) and the thin
//! C ABI in `cpp/wrapper/aec3_c_api.h` is run through `bindgen` to produce `bindings.rs`.

#![allow(non_camel_case_types, non_snake_case, non_upper_case_globals)]
#![allow(unsafe_op_in_unsafe_fn)]
#![allow(
    clippy::missing_safety_doc,
    clippy::ptr_offset_with_cast,
    clippy::too_many_arguments,
    clippy::useless_transmute
)]

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
