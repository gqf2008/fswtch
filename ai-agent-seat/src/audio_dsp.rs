use anyhow::{Result, anyhow};
use rubato::{FastFixedIn, Resampler};

/// Pipeline native sample rate: 8 kHz. Caller (sofia) audio is 8 kHz, TTS
/// (seed-tts-2.0) outputs 8 kHz, LLM accepts 8 kHz WAV — so the entire
/// pipeline (speech_buffer, ringbuf, codec, TTS, LLM WAV) runs at 8 kHz
/// to avoid unnecessary resampling. Only the VAD (earshot) requires 16 kHz,
/// handled as a bypass upsampling path.
pub const PIPELINE_SAMPLE_RATE: u32 = 8000;

/// VAD sample rate: earshot requires 16 kHz input. Caller 8 kHz audio is
/// upsampled to 16 kHz for VAD prediction only; the speech segment data
/// stays at 8 kHz (pipeline native).
pub const VAD_SAMPLE_RATE: u32 = 16000;

/// Sample rate converter using rubato.
#[allow(dead_code)]
pub struct SampleRateConverter {
    resampler: FastFixedIn<f32>,
    from_rate: u32,
    to_rate: u32,
}

impl SampleRateConverter {
    /// Create a new sample rate converter.
    ///
    /// # Arguments
    /// * `from_rate` - Source sample rate in Hz
    /// * `to_rate` - Target sample rate in Hz
    pub fn new(from_rate: u32, to_rate: u32) -> Result<Self> {
        if from_rate == 0 || to_rate == 0 {
            return Err(anyhow!("Sample rate must be non-zero"));
        }

        let resampler = FastFixedIn::<f32>::new(
            to_rate as f64 / from_rate as f64,
            2.0, // relative tolerance
            rubato::PolynomialDegree::Septic,
            1024, // chunk size
            1,    // num channels
        )?;

        Ok(Self {
            resampler,
            from_rate,
            to_rate,
        })
    }

    /// Process samples and convert sample rate.
    ///
    /// # Arguments
    /// * `samples` - Input samples at source sample rate
    ///
    /// # Returns
    /// Converted samples at target sample rate
    pub fn process(&mut self, samples: &[i16]) -> Vec<i16> {
        if samples.is_empty() {
            return Vec::new();
        }

        // Convert i16 to f32 normalized to [-1.0, 1.0]
        let float_samples: Vec<f32> = samples.iter().map(|&s| s as f32 / 32768.0).collect();

        // Process through resampler
        let waves_in = vec![float_samples];
        let waves_out = self.resampler.process(&waves_in, None).unwrap_or_default();

        if waves_out.is_empty() || waves_out[0].is_empty() {
            return Vec::new();
        }

        // Convert f32 back to i16
        waves_out[0]
            .iter()
            .map(|&s| {
                let clamped = s.clamp(-1.0, 1.0);
                (clamped * 32767.0) as i16
            })
            .collect()
    }

    /// Reset the resampler state.
    pub fn reset(&mut self) {
        self.resampler.reset();
    }
}

/// Get the codec sample rate from a FreeSWITCH session.
///
/// Reads the session's read codec and returns the implementation's actual sample rate (Hz).
/// Falls back to 8000 when the session has no read codec or the implementation pointer is null.
///
/// # Safety
/// `session_ptr` must be a valid, non-null FreeSWITCH session pointer obtained from a live
/// `fswtch::Session` (or equivalent). The caller must ensure the session remains valid for the
/// duration of this call.
/// Returns the session's read-codec sample rate (Hz), defaulting to `8000`
/// when no codec is set. Thin wrapper over `fswtch::Session::read_sample_rate`.
pub fn get_codec_rate(session: &fswtch::Session) -> u32 {
    session.read_sample_rate()
}
