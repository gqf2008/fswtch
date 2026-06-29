use anyhow::Result;
use serde::Deserialize;
use std::fs;

/// AI agent seat configuration.
///
/// Mirrors the `voice_seat.yaml` schema: an `api:` section (LLM + TTS endpoints/credentials)
/// plus an optional top-level `system_prompt`. VAD / audio / max-call-duration default when
/// absent (the YAML typically only sets `api` + `system_prompt`).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Config {
    /// LLM + TTS endpoints and credentials (the `api:` YAML section).
    pub api: ApiConfig,
    /// VAD tuning (defaults apply when the YAML omits `vad:`).
    #[serde(default)]
    pub vad: VadConfig,
    /// Audio/barge-in tuning (defaults apply when the YAML omits `audio:`).
    #[serde(default)]
    pub audio: AudioConfig,
    /// Maximum call duration in seconds (0 = unlimited).
    #[serde(default)]
    pub max_call_secs: u64,
    /// System prompt injected as the first `system` message (top-level YAML key).
    #[serde(default)]
    pub system_prompt: Option<String>,
}

/// LLM + TTS endpoints and credentials — the YAML `api:` section.
///
/// Field names match the YAML keys exactly (no `serde(rename)` needed).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ApiConfig {
    /// Pipeline mode: `"audio_llm"` (audio-native LLM, no ASR) or `"asr_llm_tts"`.
    #[serde(default)]
    pub pipeline_mode: String,
    /// LLM base URL (e.g. `https://ark.cn-beijing.volces.com/api/v3`). The chat-completions
    /// path is appended by the orchestrator.
    #[serde(default)]
    pub llm_url: String,
    /// LLM API key (bearer token).
    #[serde(default)]
    pub llm_key: String,
    /// LLM model name (e.g. `doubao-seed-2-0-mini-260428`).
    #[serde(default)]
    pub llm_model: String,
    /// Optional sampling temperature.
    pub llm_temperature: Option<f32>,
    /// Optional max output tokens.
    pub llm_max_tokens: Option<u32>,
    /// Volcano (ByteDance) TTS WebSocket API key (`X-Api-Key`).
    #[serde(default)]
    pub volcano_api_key: String,
    /// Volcano TTS resource id (`X-Api-Resource-Id`, e.g. `seed-tts-2.0`).
    #[serde(default)]
    pub volcano_resource_id: String,
    /// Volcano TTS speaker voice id.
    #[serde(default)]
    pub volcano_speaker: String,
}

/// VAD configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct VadConfig {
    /// Speech detection threshold (0.0 - 1.0).
    #[serde(default)]
    pub speech_threshold: f32,
    /// Silence timeout in milliseconds before considering speech ended.
    #[serde(default)]
    pub silence_timeout_ms: u32,
    /// Minimum RMS energy (linear f32 scale, 0.0-1.0) for a frame to be
    /// considered for speech detection. Frames below this are treated as
    /// silence regardless of VAD score. This is the primary noise gate.
    #[serde(default)]
    pub min_speech_rms: f32,
    /// Barge-in confirmation time in milliseconds.
    #[serde(default)]
    pub barge_in_confirm_ms: u32,
    /// Minimum cumulative high-probability time (ms) before speech onset fires.
    /// Uses an accumulator with decay — low-prob frames reduce the accumulator
    /// rather than resetting it. Matches voice-call's proven approach.
    #[serde(default = "default_speech_onset_ms")]
    pub speech_onset_ms: f32,
    /// Per-frame decay rate on the onset accumulator when frame prob < threshold.
    /// 0.25 means one low-prob frame cancels 25% of accumulated onset.
    #[serde(default = "default_speech_onset_decay")]
    pub speech_onset_decay: f32,
}

fn default_speech_onset_ms() -> f32 {
    80.0
}
fn default_speech_onset_decay() -> f32 {
    0.25
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            speech_threshold: 0.5,
            silence_timeout_ms: 500,
            min_speech_rms: 0.01,
            barge_in_confirm_ms: 80,
            speech_onset_ms: default_speech_onset_ms(),
            speech_onset_decay: default_speech_onset_decay(),
        }
    }
}

/// Audio configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct AudioConfig {
    /// Correlation configuration for barge-in detection.
    #[serde(default)]
    pub correlation: CorrelationConfig,
    /// Fade-out duration in milliseconds when barge-in occurs.
    #[serde(default)]
    pub fade_out_ms: u32,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            correlation: CorrelationConfig::default(),
            fade_out_ms: 80,
        }
    }
}

/// Correlation configuration for barge-in detection.
#[derive(Debug, Clone, Deserialize)]
pub struct CorrelationConfig {
    /// Correlation threshold for barge-in detection.
    #[serde(default)]
    pub threshold: f32,
    /// Window size in samples for correlation.
    #[serde(default)]
    pub window_size: usize,
}

impl Default for CorrelationConfig {
    fn default() -> Self {
        Self {
            threshold: 0.3,
            window_size: 160,
        }
    }
}

impl Config {
    /// Load configuration from a YAML file.
    pub fn load(path: &str) -> Result<Self> {
        let content = fs::read_to_string(path)?;
        let config: Config = serde_yaml::from_str(&content)?;
        Ok(config)
    }
}
