//! AI denoising stage (RNNoise via nnnoiseless).
//!
//! Suppresses non-stationary background noise (TV audio, child screams, music)
//! that traditional NS/AEC cannot handle. Inserts after up-sampling to 16 kHz
//! and before VAD, so VAD accuracy improves and the LLM receives cleaner audio.
//!
//! The nnnoiseless model expects 480-sample frames (FRAME_SIZE). At 16 kHz
//! that's 30 ms — the spectral bands are shifted vs the 48 kHz training rate,
//! but the RNN still suppresses stationary and many non-stationary noises.
//!
//! `DenoiseStage` buffers arbitrary-length input into 480-sample frames,
//! denoises each, and returns cleaned samples (with up to one frame of
//! algorithmic latency).

use nnnoiseless::DenoiseState;

/// The frame size nnnoiseless expects (480 samples).
const FRAME_SIZE: usize = nnnoiseless::FRAME_SIZE;

/// AI denoiser wrapping RNNoise (nnnoiseless). When disabled, passes audio
/// through untouched. When enabled, suppresses background noise in 480-sample
/// frames, buffering across calls.
pub struct DenoiseStage {
    enabled: bool,
    state: Box<DenoiseState<'static>>,
    /// Accumulated input waiting for a full 480-sample frame.
    in_buf: Vec<f32>,
    /// Cleaned output ready for the caller to drain.
    out_buf: Vec<f32>,
    /// Scratch buffer for process_frame output.
    frame_out: [f32; FRAME_SIZE],
}

impl DenoiseStage {
    /// Create a new stage. `enabled=false` → no-op (audio passes through).
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            state: DenoiseState::new(),
            in_buf: Vec::with_capacity(FRAME_SIZE),
            out_buf: Vec::with_capacity(FRAME_SIZE * 2),
            frame_out: [0.0; FRAME_SIZE],
        }
    }

    /// Whether denoising is active.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Feed input samples and receive cleaned output in `out`.
    ///
    /// `out` is cleared at the start of each call, so it contains only the
    /// output for the current invocation. When disabled, copies input straight
    /// to `out` (zero latency). When enabled, buffers input into 480-sample
    /// frames, denoises each, and writes the result to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        // Clear the caller's buffer — each call returns only this call's output.
        out.clear();

        if !self.enabled {
            out.extend_from_slice(input);
            return;
        }

        // Append new input to the accumulator.
        self.in_buf.extend_from_slice(input);

        // Denoise every complete 480-sample frame.
        while self.in_buf.len() >= FRAME_SIZE {
            self.state
                .process_frame(&mut self.frame_out, &self.in_buf[..FRAME_SIZE]);
            self.out_buf.extend_from_slice(&self.frame_out);
            // Drain the consumed frame, keeping the remainder.
            self.in_buf.drain(..FRAME_SIZE);
        }

        // Move all available cleaned output to the caller's buffer.
        out.append(&mut self.out_buf);
    }

    /// Reset internal state (call on barge-in / session restart to prevent
    /// ghost audio from the previous context leaking into the new one).
    pub fn reset(&mut self) {
        if self.enabled {
            self.state = DenoiseState::new();
            self.in_buf.clear();
            self.out_buf.clear();
            self.frame_out.fill(0.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_passes_through() {
        let mut stage = DenoiseStage::new(false);
        let input = vec![0.5; 480];
        let mut out = Vec::new();
        stage.process(&input, &mut out);
        assert_eq!(out, input);
    }

    #[test]
    fn enabled_produces_output_eventually() {
        let mut stage = DenoiseStage::new(true);
        // Feed 3 × 480 = 1440 samples in 160-sample chunks (simulating 10ms @ 16k).
        let mut total_out = 0;
        for _ in 0..9 {
            // 9 chunks of 160 = 1440 samples = 3 frames
            let input = vec![0.3; 160];
            let mut out = Vec::new();
            stage.process(&input, &mut out);
            total_out += out.len();
        }
        // After 3 full frames (9 × 160 = 1440 = 3 × 480), we should have
        // at least 2 frames of output (the 3rd may still be processing).
        assert!(total_out >= 2 * FRAME_SIZE, "got {total_out} samples");
    }

    #[test]
    fn reset_clears_buffers() {
        let mut stage = DenoiseStage::new(true);
        let input = vec![0.5; 480];
        let mut out = Vec::new();
        stage.process(&input, &mut out);
        assert!(!out.is_empty());
        stage.reset();
        // After reset, in_buf and out_buf should be empty.
        let mut out2 = Vec::new();
        stage.process(&[0.0; 160], &mut out2);
        // Not enough for a full frame → no output (or partial from reset state).
        assert!(out2.len() <= 160);
    }

    #[test]
    fn out_is_cleared_between_calls() {
        // Regression: the doc says `out` is cleared at the start of each call.
        // Without out.clear(), a reused `out` would accumulate across calls.
        let mut stage = DenoiseStage::new(false);
        let mut out = Vec::new();

        stage.process(&[0.5; 480], &mut out);
        assert_eq!(out.len(), 480);

        // Second call with the same `out` — must not accumulate.
        stage.process(&[0.5; 480], &mut out);
        assert_eq!(out.len(), 480, "out was not cleared between calls");
    }
}
