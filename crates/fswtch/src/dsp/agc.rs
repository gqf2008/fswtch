//! Self-developed AGC (RMS-target gain) — the AGC stage of the in-bug
//! AEC+NS+AGC pipeline.
//!
//! Why not speex's AGC (`SPEEX_PREPROCESS_SET_AGC`)? Benchmark on the
//! AEC-Challenge synthetic set: with a CORRECT far-end reference, speex AGC
//! collapses the output to near-silence (SI-SNR −12 dB, ERRE −83 dB, STOI
//! 0.35) — its gain-pumping × residual-echo-suppression coupling drives the
//! signal to zero. So AGC is self-developed here instead.
//!
//! Design: drive each frame's RMS toward `target_rms` via a one-pole-smoothed
//! gain coefficient — slow attack (rising) prevents pumping on residual noise;
//! fast release (falling) pulls gain down promptly on loud frames.

/// One-pole smoothing coefficient for gain INCREASE (attack). Small = slow.
/// ~0.02 → time-constant ≈ 50 frames (≈0.5–1 s @ 10–20 ms frames) — slow
/// enough that a brief dip into residual/noise doesn't pump the gain up.
const ATTACK_COEF: f32 = 0.02;
/// One-pole smoothing coefficient for gain DECREASE (release). Larger = fast.
/// ~0.25 → time-constant ≈ 4 frames — loud frames pull gain down promptly.
const RELEASE_COEF: f32 = 0.25;

const EPS: f32 = 1e-6;

#[derive(Debug, Clone)]
pub struct Agc {
    /// Target RMS the AGC drives toward (i16 scale, 0–32767).
    pub target_rms: f32,
    /// Maximum linear gain (ceiling). `10^(max_gain_db/20)`.
    pub max_gain: f32,
    /// Current smoothed linear gain. Starts at 1.0 (unity).
    gain: f32,
}

impl Agc {
    /// `target_rms` in i16 scale; `max_gain_db` caps the gain (e.g. 20 → 10×).
    pub fn new(target_rms: f32, max_gain_db: f32) -> Self {
        Self {
            target_rms: target_rms.max(EPS),
            max_gain: 10f32.powf(max_gain_db / 20.0),
            gain: 1.0,
        }
    }

    /// Apply AGC in-place on an i16 frame. No-op on empty slices.
    pub fn process(&mut self, pcm: &mut [i16]) {
        if pcm.is_empty() {
            return;
        }
        // Frame RMS in f64 (i64 products avoid overflow on large i16 values).
        // Not reusing dsp::rms (f32) — gain accuracy needs the extra headroom.
        let sum_sq: f64 = pcm.iter().map(|&s| (s as i64 * s as i64) as f64).sum();
        let rms = (sum_sq / pcm.len() as f64).sqrt() as f32;
        // Desired gain to reach target. Clamp floor at 0 (never invert); ceiling at max_gain
        // so residual echo / noise is not amplified beyond the ceiling.
        let desired = (self.target_rms / (rms + EPS)).clamp(0.0, self.max_gain);
        // Slow attack (rising), fast release (falling).
        let coef = if desired > self.gain {
            ATTACK_COEF
        } else {
            RELEASE_COEF
        };
        self.gain += (desired - self.gain) * coef;
        // Apply. Use f32 accumulator + per-sample clamp to i16 range.
        for s in pcm.iter_mut() {
            let v = (*s as f32) * self.gain;
            *s = v.round().clamp(-32768.0, 32767.0) as i16;
        }
    }

    /// Current linear gain (diagnostic).
    pub fn gain(&self) -> f32 {
        self.gain
    }

    /// Reset the smoothed gain to unity (e.g. on call start).
    pub fn reset(&mut self) {
        self.gain = 1.0;
    }
}

impl Default for Agc {
    fn default() -> Self {
        Self::new(3000.0, 20.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unity_on_silence() {
        // Silence: RMS 0 → desired = max_gain, but attack is slow so first
        // frame barely moves; output stays ~0 (no amplification of nothing).
        let mut agc = Agc::new(3000.0, 20.0);
        let mut pcm = vec![0i16; 160];
        agc.process(&mut pcm);
        assert!(pcm.iter().all(|&s| s == 0));
    }

    #[test]
    fn lifts_quiet_speech() {
        // A quiet speech-like frame (RMS ~100) over many frames should be
        // lifted toward target 3000, capped at max_gain (20 dB → 10×). The
        // SLOW attack means early frames barely rise; only assert after
        // convergence.
        let mut agc = Agc::new(3000.0, 20.0);
        let mut max_gain_seen = 0.0f32;
        let mut last_rms = 0.0f64;
        for _ in 0..400 {
            let mut pcm = vec![100i16; 160]; // quiet, RMS 100
            agc.process(&mut pcm);
            max_gain_seen = max_gain_seen.max(agc.gain());
            last_rms = (pcm.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / 160.0).sqrt();
        }
        // After convergence: gain ~10× (capped), output lifted far above 100.
        assert!(last_rms > 500.0, "last rms {last_rms} not lifted");
        assert!(
            (max_gain_seen - 10.0).abs() < 0.5,
            "gain {max_gain_seen} not capped at 10×"
        );
    }

    #[test]
    fn attenuates_loud_fast() {
        // Loud frame (RMS ~20000) → release pulls gain down promptly.
        let mut agc = Agc::new(3000.0, 20.0);
        let mut pcm = vec![20000i16; 160];
        agc.process(&mut pcm);
        // Gain dropped below unity quickly (release fast).
        assert!(agc.gain() < 1.0, "gain {} not pulled down", agc.gain());
    }
}
