//! Pipeline constants and shared audio type aliases.
//!
//! Formerly hosted `FarFieldParams` / `PipelineParams` (standalone-binary-era
//! DSP pipeline config). Those had no consumer — the active AEC/NS run in
//! FreeSWITCH's `preprocess` app — and were removed. What remains here are
//! the sample-rate constants (used by resampling + the media bug) and the
//! `PcmFrame` alias.
//!
//! `PIPELINE_SAMPLE_RATE` is duplicated from config to break the dependency
//! edge: config is the source of truth for YAML serialization; this constant
//! is the source of truth for runtime signal processing.

/// Pipeline audio sample rate (Hz). The DSP chain and the agent VAD run at
/// 16 kHz mono.
pub const PIPELINE_SAMPLE_RATE: u32 = 16_000;

/// Telephony (G.711) sample rate (Hz). FreeSWITCH and SIP trunks use 8 kHz.
pub const TELEPHONY_SAMPLE_RATE: u32 = 8_000;

/// One frame of mono PCM audio (i16 samples). Used in TTS playback and
/// media-bug handoff paths.
pub type PcmFrame = Vec<i16>;
