//! Kalman-filter and CUSUM change-detection link-quality estimators.
//!
//! Wraps FreeSWITCH's `switch_estimators.h` API (see
//! `crates/fswtch-src/freeswitch/src/include/switch_estimators.h`):
//!
//! - [`KalmanEstimator`] — a one-dimensional Kalman filter that smooths a noisy
//!   measurement stream (packet loss, RTT, jitter, …). Feed successive
//!   measurements in via [`KalmanEstimator::estimate`] and read the smoothed
//!   estimate out via [`KalmanEstimator::estimate_value`].
//! - [`CusumDetector`] — a CUSUM change detector layered on a Kalman filter that
//!   flags sudden shifts in a measurement stream (a slow-link condition).
//!   Feed measurements in via [`CusumDetector::detect_change`].
//! - [`is_slow_link`] — a free function that combines a loss estimator with an
//!   RTT estimator to decide whether the link has gone slow.
//!
//! Both estimators are caller-provided-storage types: FreeSWITCH hands the
//! caller a `#[repr(C)]` struct to zero-initialize and then calls
//! `switch_kalman_init` / `switch_kalman_cusum_init` on it. There are no
//! corresponding destroy functions, so the storage is simply dropped (via the
//! `Box`) when the wrapper falls out of scope. This mirrors the `codec.rs`
//! wrapper pattern.
//!
//! Neither C type is thread-safe (the estimators mutate their own state through
//! `&self`-shaped accessors), so both wrappers carry a `PhantomData<*const ()>`
//! marker and are neither `Send` nor `Sync`.

use std::marker::PhantomData;
use std::mem::MaybeUninit;

use crate::{GENERR, Result, SwitchError, sys};

/// `SWITCH_TRUE` from `fswtch-sys`, as a plain `bool` for ergonomic comparisons.
#[inline]
fn is_true(value: sys::switch_bool_t) -> bool {
    value == sys::switch_bool_t_SWITCH_TRUE
}

/// A one-dimensional Kalman filter that smooths a noisy scalar measurement.
///
/// Wraps FreeSWITCH's `kalman_estimator_t`. Allocated zeroed via `Box`, then
/// initialized with `switch_kalman_init`. There is no destroy call — the
/// `Box` frees the storage on drop.
///
/// `Q` is the process-noise covariance (how much you expect the true value to
/// wander between samples) and `R` is the measurement-noise covariance (how
/// noisy you believe each measurement is). Larger `Q` relative to `R` makes the
/// filter track the measurements more eagerly; larger `R` makes it trust its
/// prior estimate more and smooth harder.
pub struct KalmanEstimator {
    raw: Box<sys::kalman_estimator_t>,
    // `kalman_estimator_t` is not thread-safe; `estimate` mutates C state through `&self`.
    _marker: PhantomData<*const ()>,
}

impl KalmanEstimator {
    /// Creates a new Kalman estimator with process-noise `Q` and
    /// measurement-noise `R`.
    ///
    /// Both arguments are positive covariances; pass them as plain `f32`. The
    /// underlying struct is zero-initialized before `switch_kalman_init` runs,
    /// matching FreeSWITCH's contract that the caller provide zeroed storage.
    ///
    /// `switch_kalman_init` returns `void` (it cannot fail), so this only
    /// returns `Err` if the box allocation itself were to fail — which, for the
    /// current `Box::new` allocator, would abort rather than yield `Err`. The
    /// `Result` is retained for API symmetry with [`CusumDetector::new`] and the
    /// rest of the crate's fallible constructors.
    pub fn new(q: f32, r: f32) -> Result<Self> {
        // SAFETY: `kalman_estimator_t` is a plain `#[repr(C)]` struct of eight
        // `f32` fields (see bindings.rs) with no padding-only invariants; zero
        // initialization is the same operation bindgen's own `Default` impl
        // performs. The struct is about to be handed to `switch_kalman_init`,
        // which populates every field FreeSWITCH uses.
        let mut raw: Box<sys::kalman_estimator_t> =
            Box::new(unsafe { MaybeUninit::<sys::kalman_estimator_t>::zeroed().assume_init() });

        // SAFETY: `raw` is a freshly zeroed estimator struct; `Q`/`R` are plain
        // floats. `switch_kalman_init` writes into the struct's `Q`/`R` (and
        // seeds the rest of the state); it returns `void` and is always sound to
        // call on zeroed storage.
        unsafe {
            sys::switch_kalman_init(Box::as_mut(&mut raw), q, r);
        }

        Ok(Self {
            raw,
            _marker: PhantomData,
        })
    }

    /// The raw `kalman_estimator_t` pointer, for escape-hatch FFI.
    #[inline]
    pub(crate) fn as_ptr(&self) -> *mut sys::kalman_estimator_t {
        std::ptr::addr_of!(*self.raw) as *mut sys::kalman_estimator_t
    }

    /// Runs one Kalman step: folds `measurement` into the estimate and returns
    /// `true` on success.
    ///
    /// `system_model` selects the system model used for this prediction step.
    /// FreeSWITCH's header (`switch_estimators.h`) does not enumerate the
    /// accepted values, so this passes the integer straight through as an
    /// `i32`; callers should consult the FreeSWITCH sources for the model
    /// constants in use. Typical values observed in the tree are `0` and `1`.
    ///
    /// Returns `false` when `switch_kalman_estimate` reports `SWITCH_FALSE`.
    ///
    /// Takes `&mut self` because the Kalman step mutates the estimator's internal state
    /// (`val_estimate`/`P`/`K`); the exclusive borrow prevents aliasing with concurrent reads
    /// of the estimator's fields.
    pub fn estimate(&mut self, measurement: f32, system_model: i32) -> bool {
        // SAFETY: `self.raw` is a live, initialized estimator box; the box's
        // pointer is valid for the duration of the call. `measurement` is a plain
        // float and `system_model` is a plain `c_int`.
        let rc = unsafe {
            sys::switch_kalman_estimate(
                std::ptr::addr_of!(*self.raw) as *mut sys::kalman_estimator_t,
                measurement,
                system_model as std::os::raw::c_int,
            )
        };
        is_true(rc)
    }

    /// The most recently produced Kalman estimate (the smoothed value).
    ///
    /// This is a snapshot of the `val_estimate` field; call [`estimate`](Self::estimate)
    /// first to advance the filter. Before any measurement has been fed in, this
    /// is the zero-initialized value (`0.0`).
    #[inline]
    pub fn estimate_value(&self) -> f32 {
        // SAFETY: `self.raw` is a live box; reading a plain `f32` field through
        // a shared reference is sound (no interior mutability hazard on the Rust
        // side — FreeSWITCH mutates it only via `estimate`, which takes `&self`
        // here precisely because the C object is single-threaded).
        self.raw.val_estimate
    }

    /// The most recently measured (noisy) value fed into the filter.
    #[inline]
    pub fn measured_value(&self) -> f32 {
        self.raw.val_measured
    }

    /// The configured process-noise covariance `Q`.
    #[inline]
    pub fn q(&self) -> f32 {
        self.raw.Q
    }

    /// The configured measurement-noise covariance `R`.
    #[inline]
    pub fn r(&self) -> f32 {
        self.raw.R
    }
}

impl std::fmt::Debug for KalmanEstimator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KalmanEstimator")
            .field("estimate", &self.estimate_value())
            .field("measured", &self.measured_value())
            .field("Q", &self.q())
            .field("R", &self.r())
            .finish()
    }
}

/// A CUSUM change detector layered on a Kalman filter.
///
/// Wraps FreeSWITCH's `cusum_kalman_detector_t`. Allocated zeroed via `Box`,
/// then initialized with `switch_kalman_cusum_init`. There is no destroy call —
/// the `Box` frees the storage on drop.
///
/// `epsilon` is the minimum mean shift the detector is sensitive to, and `h`
/// is the detection threshold (the accumulated CUSUM statistic must exceed `h`
/// for a change to be reported). Larger `h` reduces false positives at the cost
/// of detection latency; larger `epsilon` makes the detector sensitive only to
/// larger jumps.
pub struct CusumDetector {
    raw: Box<sys::cusum_kalman_detector_t>,
    // `cusum_kalman_detector_t` is not thread-safe; `detect_change` mutates C state
    // through `&self`.
    _marker: PhantomData<*const ()>,
}

impl CusumDetector {
    /// Creates a new CUSUM detector tuned for a minimum mean shift `epsilon`
    /// and detection threshold `h`.
    ///
    /// The underlying struct is zero-initialized before `switch_kalman_cusum_init`
    /// runs, matching FreeSWITCH's contract. Returns
    /// [`crate::SwitchError`](`GENERR`) when `switch_kalman_cusum_init` returns
    /// `SWITCH_FALSE`.
    pub fn new(epsilon: f32, h: f32) -> Result<Self> {
        // SAFETY: `cusum_kalman_detector_t` is a plain `#[repr(C)]` struct of
        // `f32` fields (see bindings.rs) with no padding-only invariants; zero
        // initialization is the same operation bindgen's own `Default` impl
        // performs. The struct is about to be handed to
        // `switch_kalman_cusum_init`, which populates every field FreeSWITCH
        // uses.
        let mut raw: Box<sys::cusum_kalman_detector_t> = Box::new(unsafe {
            MaybeUninit::<sys::cusum_kalman_detector_t>::zeroed().assume_init()
        });

        // SAFETY: `raw` is a freshly zeroed detector struct; `epsilon`/`h` are
        // plain floats. `switch_kalman_cusum_init` seeds the detector's constants
        // and returns `SWITCH_TRUE` on success.
        let rc = unsafe { sys::switch_kalman_cusum_init(Box::as_mut(&mut raw), epsilon, h) };
        if !is_true(rc) {
            return Err(SwitchError(GENERR));
        }

        Ok(Self {
            raw,
            _marker: PhantomData,
        })
    }

    /// The raw `cusum_kalman_detector_t` pointer, for escape-hatch FFI.
    #[inline]
    pub(crate) fn as_ptr(&self) -> *mut sys::cusum_kalman_detector_t {
        std::ptr::addr_of!(*self.raw) as *mut sys::cusum_kalman_detector_t
    }

    /// Feeds one `measurement` (with running `rtt_avg` context) into the
    /// detector and returns `true` when a change (slow-link condition) is
    /// detected.
    ///
    /// `switch_kalman_cusum_detect_change` returns `SWITCH_TRUE` exactly when the
    /// accumulated CUSUM statistic crosses the configured `h` threshold.
    ///
    /// Takes `&mut self` because the CUSUM step mutates the detector's internal state.
    pub fn detect_change(&mut self, measurement: f32, rtt_avg: f32) -> bool {
        // SAFETY: `self.raw` is a live, initialized detector box; the box's
        // pointer is valid for the duration of the call. `measurement`/`rtt_avg`
        // are plain floats.
        let rc = unsafe {
            sys::switch_kalman_cusum_detect_change(
                std::ptr::addr_of!(*self.raw) as *mut sys::cusum_kalman_detector_t,
                measurement,
                rtt_avg,
            )
        };
        is_true(rc)
    }

    /// The configured minimum mean shift `epsilon`.
    #[inline]
    pub fn epsilon(&self) -> f32 {
        self.raw.epsilon
    }

    /// The configured detection threshold `h`.
    #[inline]
    pub fn h(&self) -> f32 {
        self.raw.h
    }
}

impl std::fmt::Debug for CusumDetector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CusumDetector")
            .field("epsilon", &self.epsilon())
            .field("h", &self.h())
            .finish()
    }
}

/// Returns `true` when the link is judged slow, combining the packet-loss
/// estimator `est_loss` with the round-trip-time estimator `est_rtt`.
///
/// Wraps FreeSWITCH's `switch_kalman_is_slow_link`, which returns `SWITCH_TRUE`
/// when both estimators agree the link has degraded. Both estimators are read
/// (not advanced) by this call — feed them measurements via
/// [`KalmanEstimator::estimate`] beforehand.
pub fn is_slow_link(est_loss: &KalmanEstimator, est_rtt: &KalmanEstimator) -> bool {
    // SAFETY: both estimators are live, initialized boxes; their pointers are
    // valid for the duration of the (read-only) call.
    let rc = unsafe { sys::switch_kalman_is_slow_link(est_loss.as_ptr(), est_rtt.as_ptr()) };
    is_true(rc)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Type-level smoke check that the wrappers compile and the boolean helper
    /// maps the `switch_bool_t` constants correctly. No live estimator is
    /// constructed here (that needs the FreeSWITCH runtime), so the FFI is
    /// exercised only at runtime.
    #[test]
    fn bool_constant_mapping_is_correct() {
        assert!(is_true(sys::switch_bool_t_SWITCH_TRUE));
        assert!(!is_true(sys::switch_bool_t_SWITCH_FALSE));
    }
}
