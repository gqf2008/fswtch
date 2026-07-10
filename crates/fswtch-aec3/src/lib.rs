//! Safe Rust bindings over WebRTC AEC3 (acoustic echo cancellation).
//!
//! AEC3 is built from the vendored C++ tree via the FFI bridge in [`fswtch-aec3-sys`]; this crate
//! wraps the raw `extern "C"` ABI in owned, RAII handles that follow the `fswtch` conventions
//! ([`NonNull`] handles, [`Drop`] frees the C object, `# Safety` contracts on public `unsafe fn`).
//!
//! This is the early scaffold: the C++ toolchain is wired up (Phase 0) and the Ooura 128-point
//! FFT closure compiles and links (Phase 1). The full [`EchoCanceller3`] surface is added in
//! later phases as the AEC3 C++ closure is vendored and the thin C ABI is filled in.

pub use fswtch_aec3_sys as sys;

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
