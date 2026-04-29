#![allow(non_camel_case_types, non_snake_case, non_upper_case_globals)]
#![cfg_attr(feature = "bindgen", allow(unsafe_op_in_unsafe_fn))]
#![cfg_attr(
    feature = "bindgen",
    allow(
        clippy::missing_safety_doc,
        clippy::ptr_offset_with_cast,
        clippy::too_many_arguments,
        clippy::useless_transmute
    )
)]

#[cfg(feature = "bindgen")]
include!(concat!(env!("OUT_DIR"), "/bindings.rs"));

#[cfg(not(feature = "bindgen"))]
mod fallback;

#[cfg(not(feature = "bindgen"))]
pub use fallback::*;
