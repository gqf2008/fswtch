//! Safe Rust bindings over WebRTC audio processing (AEC3 + HF + NS + AGC2).
//!
//! Each module is built from the vendored C++ tree via the FFI bridge in `fswtch-apm-sys`; this
//! crate wraps the raw `extern "C"` ABI in owned, RAII handles that follow the `fswtch` conventions
//! ([`std::ptr::NonNull`] handles, [`Drop`] frees the C object, `# Safety` contracts on public `unsafe fn`).
//!
//! Status: AEC3 + HF + NS + AGC2 are all exposed, plus the chained `mod_apm` FreeSWITCH module.
//! The lower-level `api_version`/`ooura_smoke` entrypoints remain as pipeline smoke checks.

pub use fswtch_apm_sys as sys;

mod aec3;
/// Shared error type (alias of [`Aec3Error`](crate::aec3::Aec3Error)) used by every module.
pub use aec3::Aec3Error as Error;
pub(crate) use aec3::check;
pub use aec3::*;

mod hpf;
pub use hpf::*;

mod ns;
pub use ns::*;

mod agc2;
pub use agc2::*;

/// Returns the version of the AEC3 C ABI exposed by the bundled C++.
///
/// Exists to prove the `cmake -> static lib -> bindgen -> Rust` pipeline end to end.
pub fn api_version() -> i32 {
    // SAFETY: `fswtch_aec3_api_version` is a pure function taking no pointer arguments and
    // performing no I/O; it has no preconditions.
    unsafe { sys::fswtch_aec3_api_version() }
}

/// Runs one forward Ooura 128-point FFT over a zero buffer through the vendored C++ closure.
///
/// Exists to prove the ooura C++ closure compiles and links in scalar (portable) mode (Phase 1).
/// The FFT is driven internally by AEC3 in later phases; this entrypoint just exercises linkage.
pub fn ooura_smoke() -> i32 {
    // SAFETY: `fswtch_aec3_ooura_smoke` takes no pointer arguments; it operates on a stack buffer
    // owned by the C++ side and performs no I/O.
    unsafe { sys::fswtch_aec3_ooura_smoke() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_version_links_and_runs() {
        assert_eq!(api_version(), 1);
    }

    #[test]
    fn ooura_smoke_links_and_runs() {
        assert_eq!(ooura_smoke(), 1);
    }
}
