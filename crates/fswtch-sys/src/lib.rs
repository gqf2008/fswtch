#![allow(non_camel_case_types, non_snake_case, non_upper_case_globals)]
#![cfg_attr(feature = "bindgen", allow(unsafe_op_in_unsafe_fn))]

#[cfg(feature = "bindgen")]
include!(concat!(env!("OUT_DIR"), "/bindings.rs"));

#[cfg(not(feature = "bindgen"))]
mod fallback;

#[cfg(not(feature = "bindgen"))]
pub use fallback::*;
