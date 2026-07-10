//! Safe Rust bindings over WebRTC AEC3 (acoustic echo cancellation).
//!
//! AEC3 is built from the vendored C++ tree via the FFI bridge in [`fswtch-aec3-sys`]; this crate
//! wraps the raw `extern "C"` ABI in owned, RAII handles that follow the `fswtch` conventions
//! ([`NonNull`] handles, [`Drop`] frees the C object, `# Safety` contracts on public `unsafe fn`).
//!
//! This is the Phase 0 scaffold: the C++ toolchain is wired up and a smoke entrypoint links and
//! runs, but the full [`EchoCanceller3`] surface is added in later phases as the AEC3 C++ closure
//! is vendored and the thin C ABI is filled in.

pub use fswtch_aec3_sys as sys;

/// Returns the version of the AEC3 C ABI exposed by the bundled C++.
///
/// Exists to prove the `cmake -> static lib -> bindgen -> Rust` pipeline end to end.
pub fn api_version() -> i32 {
    // SAFETY: `fswtch_aec3_api_version` is a pure function taking no pointer arguments and
    // performing no I/O; it has no preconditions.
    unsafe { sys::fswtch_aec3_api_version() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_version_links_and_runs() {
        assert_eq!(api_version(), 1);
    }
}
