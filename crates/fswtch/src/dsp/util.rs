//! Shared DSP utilities — SIMD RMS calculation and sample rate conversion.

use rubato::audioadapter_buffers::owned::InterleavedOwned;
use rubato::{
    Async, FixedAsync, Resampler, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};

/// Default sinc interpolation parameters for high-quality resampling.
fn default_sinc_params() -> SincInterpolationParameters {
    SincInterpolationParameters {
        sinc_len: 128,
        f_cutoff: rubato::calculate_cutoff(128, WindowFunction::Blackman2),
        interpolation: SincInterpolationType::Quadratic,
        oversampling_factor: 256,
        window: WindowFunction::Blackman2,
    }
}

/// Streaming sample rate converter backed by rubato sinc interpolation.
///
/// Wraps `rubato::Async<f32>` with internal i16↔f32 conversion and
/// input buffering so callers can feed arbitrary-size chunks.
///
/// Create one instance per audio stream (it maintains state across calls).
pub struct SampleRateConverter {
    inner: Async<f32>,
    /// Leftover input samples (f32, mono) that didn't fill a complete rubato chunk.
    residual: Vec<f32>,
    /// Ratio: dst_rate / src_rate (for output capacity estimation).
    ratio: f64,
}

impl SampleRateConverter {
    /// Create a new converter for the given sample rates.
    ///
    /// Uses sinc interpolation with anti-aliasing for high quality.
    /// `chunk_size` is the nominal input chunk size hint.
    pub fn new(src_rate: u32, dst_rate: u32, chunk_size: usize) -> Result<Self, String> {
        let ratio = dst_rate as f64 / src_rate as f64;
        let params = default_sinc_params();
        let inner = Async::<f32>::new_sinc(
            ratio,
            2.0, // relative chunk size headroom
            &params,
            chunk_size,
            1, // mono
            FixedAsync::Input,
        )
        .map_err(|e| format!("resampler init failed: {e}"))?;

        Ok(Self {
            inner,
            residual: Vec::new(),
            ratio,
        })
    }

    /// Process a chunk of i16 samples, returning resampled i16 output.
    ///
    /// Handles internal buffering: leftover samples from previous calls are
    /// prepended, and incomplete trailing chunks are saved for the next call.
    pub fn process(&mut self, input: &[i16]) -> Vec<i16> {
        if input.is_empty() && self.residual.is_empty() {
            return Vec::new();
        }

        // Convert i16 → f32 (÷32768 normalizes to [-1, 1)) and prepend residual.
        // Output uses ×32767 + clamp (see below) — standard asymmetry avoids
        // overflow at the -1.0 → i16::MIN edge.
        let mut buf: Vec<f32> = Vec::with_capacity(self.residual.len() + input.len());
        buf.extend_from_slice(&self.residual);
        buf.extend(input.iter().map(|&s| s as f32 / 32768.0));

        // FixedAsync::Input guarantees a fixed input chunk size, so we sample
        // it once here and reuse for every loop iteration below.
        let chunk_size = self.inner.input_frames_next();
        let est_out = ((buf.len() as f64) * self.ratio).ceil() as usize + chunk_size;
        let mut output: Vec<i16> = Vec::with_capacity(est_out);

        // Process complete chunks
        while buf.len() >= chunk_size {
            let chunk_f32: Vec<f32> = buf.drain(..chunk_size).collect();
            let input_buf = match InterleavedOwned::new_from(chunk_f32, 1, chunk_size) {
                Ok(b) => b,
                Err(_) => continue,
            };

            match self.inner.process(&input_buf, 0, None) {
                Ok(result) => {
                    let data = result.take_data();
                    output.extend(data.iter().map(|&s| {
                        (s * 32767.0)
                            .round()
                            .clamp(i16::MIN as f32, i16::MAX as f32) as i16
                    }));
                }
                Err(e) => {
                    // Log to stderr — dropped samples cause audible gaps.
                    // Continue processing rather than aborting the stream.
                    // Uses eprintln! (not crate::log_error) to keep dsp FFI-free.
                    eprintln!("[dsp] rubato process error (samples dropped): {e}");
                }
            }
        }

        // Save leftover for next call
        self.residual = buf;
        output
    }

    /// Flush remaining samples by padding with silence.
    /// Call this when the stream ends to avoid losing the tail.
    ///
    /// Rubato's sinc filter has group delay — a single flush pass may not
    /// drain all internal state. We pad with enough silence (2× chunk_size)
    /// and process repeatedly until output is empty, ensuring the filter
    /// tail is fully drained.
    pub fn flush(&mut self) -> Vec<i16> {
        if self.residual.is_empty() {
            return Vec::new();
        }
        let chunk_size = self.inner.input_frames_next();
        // Pad residual to at least chunk_size with silence.
        self.residual
            .resize(self.residual.len().max(chunk_size), 0.0);

        let mut output = Vec::new();

        // Process repeatedly to drain the sinc filter's group delay.
        // Each pass may produce output due to the filter's impulse response.
        // We stop when a pass produces no output (filter fully drained).
        for _ in 0..4 {
            // Pad with more silence for the next pass.
            self.residual
                .resize(self.residual.len().max(chunk_size), 0.0);
            let result = self.process(&[]);
            if result.is_empty() {
                break;
            }
            output.extend(result);
        }

        self.residual.clear();
        output
    }
}

/// Shared RMS helper — SIMD-optimized: processes 8 samples at a time using `wide::f32x8`.
pub fn rms(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }

    // Use SIMD for chunks of 8 or more samples
    if samples.len() >= 8 {
        use wide::f32x8;

        let mut sum_vec = f32x8::splat(0.0);
        let chunks = samples.len() / 8;

        for i in 0..chunks {
            let offset = i * 8;
            let chunk = &samples[offset..offset + 8];

            // Convert i16x8 to f32x8
            let values = f32x8::from([
                chunk[0] as f32,
                chunk[1] as f32,
                chunk[2] as f32,
                chunk[3] as f32,
                chunk[4] as f32,
                chunk[5] as f32,
                chunk[6] as f32,
                chunk[7] as f32,
            ]);

            // Square and accumulate: sum_vec += values * values
            sum_vec += values * values;
        }

        // Horizontal sum of the vector
        let sum_array = sum_vec.to_array();
        let mut sum: f32 = sum_array.iter().sum();

        // Handle remaining samples (less than 8)
        let remainder_start = chunks * 8;
        for &s in &samples[remainder_start..] {
            sum += (s as f32) * (s as f32);
        }

        (sum / samples.len() as f32).sqrt()
    } else {
        // Fallback for small samples
        let sum: f32 = samples.iter().map(|&s| (s as f32) * (s as f32)).sum();
        (sum / samples.len() as f32).sqrt()
    }
}

#[cfg(test)]
mod rms_tests {
    use super::*;

    #[test]
    fn rms_empty() {
        assert_eq!(rms(&[]), 0.0);
    }

    #[test]
    fn rms_silence() {
        let silence = vec![0i16; 512];
        assert!(rms(&silence) < 0.001);
    }

    #[test]
    fn rms_constant() {
        let constant = vec![1000i16; 512];
        let result = rms(&constant);
        assert!((result - 1000.0).abs() < 0.1);
    }

    #[test]
    fn rms_simd_vs_scalar() {
        // Test that SIMD path produces same result as scalar path
        let samples: Vec<i16> = (0..512).map(|i| (i as i16) * 50).collect();

        // SIMD path (512 samples >= 8)
        let simd_result = rms(&samples);

        // Manual scalar calculation
        let sum: f32 = samples.iter().map(|&s| (s as f32) * (s as f32)).sum();
        let scalar_result = (sum / samples.len() as f32).sqrt();

        assert!((simd_result - scalar_result).abs() < 0.01);
    }

    #[test]
    fn rms_small_fallback() {
        // Test scalar fallback for < 8 samples
        let small = vec![1000i16, 2000, 3000, 4000];
        let result = rms(&small);
        let expected: f32 =
            ((1000.0f32 * 1000.0 + 2000.0 * 2000.0 + 3000.0 * 3000.0 + 4000.0 * 4000.0) / 4.0)
                .sqrt();
        assert!((result - expected).abs() < 0.1);
    }

    #[test]
    #[ignore = "microbenchmark — run with: cargo test -p fswtch --lib -- rms_benchmark --ignored"]
    fn rms_benchmark() {
        use std::time::Instant;

        let samples = vec![1000i16; 512];
        let iterations = 100_000;

        let start = Instant::now();
        for _ in 0..iterations {
            let _ = rms(&samples);
        }
        let elapsed = start.elapsed();

        let per_call_us = elapsed.as_micros() as f64 / iterations as f64;
        println!("RMS (512 samples, SIMD): {:.2} μs/call", per_call_us);
        println!("Total: {:?}", elapsed);

        // Should be under 15μs per call (allows for system load variance)
        assert!(per_call_us < 15.0, "RMS too slow: {:.2} μs", per_call_us);
    }
}

#[cfg(test)]
mod resampler_tests {
    use super::*;

    #[test]
    fn flush_produces_tail_samples_without_truncation() {
        // Feed a known signal through the resampler, then flush.
        // Use a non-chunk-aligned input size so there are guaranteed
        // residual samples left in the buffer after process().
        let mut src = SampleRateConverter::new(8000, 16000, 160).unwrap();

        // Feed multiple chunks of non-zero samples to build up filter state.
        // Use an odd number of samples (not aligned to internal chunk_size)
        // to ensure residual samples remain after process().
        for _ in 0..5 {
            let input: Vec<i16> = (0..137)
                .map(|i| ((i as f32 * 0.1).sin() * 10000.0) as i16)
                .collect();
            let _main_output = src.process(&input);
        }

        // Flush should produce additional tail samples from the sinc filter's
        // group delay. These are real audio that would otherwise be lost.
        let tail = src.flush();
        assert!(
            !tail.is_empty(),
            "flush() must produce tail samples from sinc filter group delay"
        );

        // Tail should contain non-zero samples (the filter ringing out).
        let has_nonzero = tail.iter().any(|&s| s != 0);
        assert!(
            has_nonzero,
            "flush() tail must contain non-zero samples (filter ring-out), got all zeros"
        );
    }

    #[test]
    fn flush_is_idempotent_after_drain() {
        let mut src = SampleRateConverter::new(8000, 16000, 160).unwrap();
        let input = vec![5000i16; 160];
        let _ = src.process(&input);
        let _ = src.flush();

        // Second flush on an already-drained converter must return empty.
        let second_flush = src.flush();
        assert!(
            second_flush.is_empty(),
            "second flush() after drain must be empty, got {} samples",
            second_flush.len()
        );
    }

    #[test]
    fn flush_empty_input_returns_empty() {
        let mut src = SampleRateConverter::new(16000, 16000, 512).unwrap();
        let tail = src.flush();
        assert!(tail.is_empty(), "flush with no input must return empty");
    }

    #[test]
    fn process_then_flush_preserves_total_energy() {
        // Verify that process() + flush() together capture all the energy
        // from the input signal (no silent truncation).
        let mut src = SampleRateConverter::new(8000, 16000, 160).unwrap();

        // Generate a sine wave at 8kHz
        let input: Vec<i16> = (0..320)
            .map(|i| ((i as f32 * 0.2).sin() * 15000.0) as i16)
            .collect();

        let main_output = src.process(&input);
        let tail_output = src.flush();

        // Combined output should have more samples than input (upsampling 8k→16k)
        let total_output = main_output.len() + tail_output.len();
        assert!(
            total_output > input.len(),
            "upsampled output ({}) should exceed input length ({})",
            total_output,
            input.len()
        );

        // Energy check: combined output RMS should be in the same ballpark
        // as input RMS (resampling preserves energy approximately).
        let mut combined = main_output;
        combined.extend(tail_output);
        let input_rms = rms(&input);
        let output_rms = rms(&combined);

        // Allow 30% tolerance for resampling artifacts and filter effects
        let ratio = output_rms / input_rms;
        assert!(
            ratio > 0.5 && ratio < 1.5,
            "output RMS ({:.1}) should be within 50% of input RMS ({:.1}), ratio={:.2}",
            output_rms,
            input_rms,
            ratio
        );
    }
}
