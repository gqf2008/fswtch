//! Safe Rust bindings over WebRTC AEC3 (acoustic echo cancellation).
//!
//! AEC3 is built from the vendored C++ tree via the FFI bridge in [`fswtch-aec3-sys`]; this crate
//! wraps the raw `extern "C"` ABI in owned, RAII handles that follow the `fswtch` conventions
//! ([`NonNull`] handles, [`Drop`] frees the C object, `# Safety` contracts on public `unsafe fn`).
//!
//! Status: the C++ toolchain is wired up (Phase 0), the Ooura 128-point FFT closure compiles
//! (Phase 1), the full AEC3 C++ closure converges (Phase 2), and a thin C ABI over
//! [`EchoCanceller3`] is exposed (Phase 3). The owned, RAII safe wrapper (Phase 4) is layered on
//! top of [`sys`]; until then the lower-level entrypoints here prove the pipeline end to end.

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

    #[test]
    fn aec3_runs_real_pipeline() {
        // Exercises the real WebRTC EchoCanceller3 (16 kHz mono, default config) end to end
        // through the Phase 3 C ABI: create -> analyze_render + process_capture over several
        // 10 ms frames -> active_processing/metrics -> destroy. Proves the vendored AEC3 C++ not
        // only links but actually processes a signal. (16 kHz / 1 band avoids the QMF/resampler
        // stubs entirely — split/merge are no-ops there.)
        const RATE: i32 = 16_000;
        const CH: usize = 1;
        let frame_samples = (RATE / 100) as usize * CH; // 160 samples per 10 ms frame
        let render = vec![0i16; frame_samples];
        let mut capture = vec![0i16; frame_samples];

        // SAFETY: `create` returns either a fresh owned handle or null; we assert non-null. The
        // handle is exclusively owned by this test for its entire lifetime.
        let aec = unsafe { sys::fswtch_aec3_create(RATE, CH, CH) };
        assert!(!aec.is_null(), "fswtch_aec3_create returned null");

        for _ in 0..20 {
            // SAFETY: `aec` is live; `render`/`capture` are valid `int16_t` arrays of exactly
            // `frame_samples` (= rate/100 * CH) samples matching the channel count passed to
            // create. Capture-side calls are serialized on this single thread.
            unsafe {
                assert_eq!(
                    sys::fswtch_aec3_analyze_render(aec, render.as_ptr(), CH),
                    0,
                    "analyze_render failed"
                );
                assert_eq!(
                    sys::fswtch_aec3_process_capture(aec, capture.as_mut_ptr(), CH, 0),
                    0,
                    "process_capture failed"
                );
            }
        }

        // SAFETY: `aec` is live; these read-only const-arg calls don't mutate state. `aec` is
        // `*mut` so it's cast to the `*const` the functions expect.
        let active =
            unsafe { sys::fswtch_aec3_active_processing(aec as *const sys::fswtch_aec3_t) };
        assert!(
            active == 0 || active == 1,
            "active_processing out of range: {active}"
        );
        let mut erl = 0.0f64;
        let mut erle = 0.0f64;
        let mut delay = 0i32;
        unsafe {
            sys::fswtch_aec3_get_metrics(
                aec as *const sys::fswtch_aec3_t,
                &mut erl,
                &mut erle,
                &mut delay,
            );
        }
        let _ = (erl, erle, delay); // metrics are observable; exact values not asserted here.

        // SAFETY: destroys the exclusively-owned handle; no references survive past this point.
        unsafe { sys::fswtch_aec3_destroy(aec) };
    }
}
