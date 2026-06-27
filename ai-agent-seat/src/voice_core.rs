use anyhow::Result;
use serde::Deserialize;
use std::fs;

/// AI agent seat configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// AI endpoints configuration.
    pub ai: AiConfig,
    /// VAD configuration.
    pub vad: VadConfig,
    /// Audio configuration.
    pub audio: AudioConfig,
    /// Maximum call duration in seconds (0 = unlimited).
    pub max_call_secs: u64,
}

/// AI endpoints configuration.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AiConfig {
    /// ASR endpoint (e.g., "unimrcp:zh-CN").
    pub asr_endpoint: String,
    /// LLM API endpoint URL.
    pub llm_endpoint: String,
    /// LLM API key.
    pub llm_api_key: String,
    /// LLM model name.
    pub llm_model: String,
    /// TTS endpoint (e.g., "elevenlabs:voice-id").
    pub tts_endpoint: String,
    /// TTS API key.
    pub tts_api_key: String,
    /// System prompt for LLM.
    pub system_prompt: Option<String>,
}

/// VAD configuration.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct VadConfig {
    /// Speech detection threshold (0.0 - 1.0).
    pub speech_threshold: f32,
    /// Silence timeout in milliseconds before considering speech ended.
    pub silence_timeout_ms: u32,
    /// Sample rate for VAD processing (typically 16000).
    pub sample_rate: u32,
    /// Minimum speech RMS to consider as speech.
    pub min_speech_rms: f32,
    /// Barge-in confirmation time in milliseconds.
    pub barge_in_confirm_ms: u32,
}

/// Audio configuration.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AudioConfig {
    /// Correlation configuration for barge-in detection.
    pub correlation: CorrelationConfig,
    /// Fade-out duration in milliseconds when barge-in occurs.
    pub fade_out_ms: u32,
}

/// Correlation configuration for barge-in detection.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CorrelationConfig {
    /// Correlation threshold for barge-in detection.
    pub threshold: f32,
    /// Window size in samples for correlation.
    pub window_size: usize,
}

impl Config {
    /// Load configuration from a YAML file.
    pub fn load(path: &str) -> Result<Self> {
        let content = fs::read_to_string(path)?;
        let config: Config = serde_yaml::from_str(&content)?;
        Ok(config)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ai: AiConfig {
                asr_endpoint: "unimrcp:zh-CN".to_string(),
                llm_endpoint: "https://api.openai.com/v1/chat/completions".to_string(),
                llm_api_key: String::new(),
                llm_model: "gpt-4".to_string(),
                tts_endpoint: "elevenlabs:voice-id".to_string(),
                tts_api_key: String::new(),
                system_prompt: None,
            },
            vad: VadConfig {
                speech_threshold: 0.5,
                silence_timeout_ms: 500,
                sample_rate: 16000,
                min_speech_rms: 0.01,
                barge_in_confirm_ms: 80,
            },
            audio: AudioConfig {
                correlation: CorrelationConfig {
                    threshold: 0.3,
                    window_size: 160,
                },
                fade_out_ms: 80,
            },
            max_call_secs: 0,
        }
    }
}
