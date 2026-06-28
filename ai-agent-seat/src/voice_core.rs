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
    /// TTS server output sample rate (Hz); resampled to the 16 kHz pipeline internally.
    #[serde(default = "default_tts_sample_rate")]
    pub volcano_tts_sample_rate: u32,
}

fn default_tts_sample_rate() -> u32 {
    16000
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
    /// Sample rate for VAD processing (typically 16000).
    #[serde(default)]
    pub sample_rate: u32,
    /// Minimum speech RMS to consider as speech.
    #[serde(default)]
    pub min_speech_rms: f32,
    /// Barge-in confirmation time in milliseconds.
    #[serde(default)]
    pub barge_in_confirm_ms: u32,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            speech_threshold: 0.5,
            silence_timeout_ms: 500,
            sample_rate: 16000,
            min_speech_rms: 0.01,
            barge_in_confirm_ms: 80,
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
