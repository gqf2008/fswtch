//! Factory functions for building LLM and TTS providers from configuration.

use crate::doubao_responses::DoubaoResponsesLlm;
use crate::providers::{
    TtsProvider,
    mimo_tts::{MimoTtsConfig, MimoTtsProvider},
    volcano_tts::VolcanoTtsProvider,
};
use crate::voice_core::ApiConfig;
use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderValue};
use rig_core::providers::openai;
use std::sync::Arc;

/// Build an LLM provider from API configuration.
///
/// Uses the Doubao Responses API (`/responses`) via raw HTTP — ~2-3s faster
/// than Chat Completions for audio-native multimodal turns. Supports
/// streaming + tool calls simultaneously (text deltas + function call arg
/// deltas in one stream).
pub fn build_llm_provider(config: &ApiConfig) -> Result<DoubaoResponsesLlm> {
    if config.llm_base_url.is_empty() || config.llm_key.is_empty() {
        anyhow::bail!("LLM URL or key is not configured");
    }

    Ok(DoubaoResponsesLlm::new(
        config.llm_base_url.clone(),
        config.llm_key.clone(),
        config.llm_model.clone(),
        config.llm_temperature.map(|t| t as f64),
        config.llm_max_tokens.map(|m| m as u64),
    ))
}

/// Build a transcription provider from API configuration.
///
/// Uses rig's OpenAI Whisper API for speech-to-text.
pub fn build_transcription_provider(config: &ApiConfig) -> Result<openai::TranscriptionModel> {
    if config.llm_base_url.is_empty() || config.llm_key.is_empty() {
        anyhow::bail!("LLM URL or key is not configured");
    }

    // Build custom HTTP headers
    let mut headers = HeaderMap::new();

    // Add auth header based on mode
    match config.llm_auth_mode.as_str() {
        "api-key" => {
            headers.insert(
                "api-key",
                HeaderValue::from_str(&config.llm_key).context("Invalid api-key header value")?,
            );
        }
        _ => {
            // Default: uses Authorization: Bearer
        }
    }

    // Create OpenAI client with custom base URL and headers
    let client = openai::Client::builder()
        .api_key(&config.llm_key)
        .base_url(&config.llm_base_url)
        .http_headers(headers)
        .build()
        .context("Failed to build OpenAI client for transcription")?;

    // Create transcription model (use whisper-1 or configured model)
    let model_name = if config.asr_model.is_empty() {
        "whisper-1".to_string()
    } else {
        config.asr_model.clone()
    };

    Ok(openai::TranscriptionModel::new(client, model_name))
}

/// Build a TTS provider from API configuration.
///
/// Returns a boxed trait object since Volcano and MIMO have different implementations.
/// `tts_provider` empty/`"volcano"` → Volcano (default); `"mimo"` → MIMO HTTP TTS.
///
/// `on_audio` is the call-wide ringbuf producer callback — Volcano's
/// fire-and-forget driver pushes PCM chunks through it directly (audio is
/// played as it arrives, not returned from `synthesize`).
pub fn build_tts_provider(
    config: &ApiConfig,
    call_uuid: &str,
    on_audio: crate::audio_dsp::OnAudio,
    on_turn_end: crate::tts::OnTurnEnd,
) -> Result<Arc<dyn TtsProvider>> {
    match config.tts_provider.as_str() {
        "mimo" => {
            if config.llm_base_url.is_empty() {
                anyhow::bail!("MIMO TTS requires LLM URL to be configured");
            }

            let tts_config = MimoTtsConfig {
                api_key: config.llm_key.clone(),
                base_url: config.llm_base_url.clone(),
                voice: config.mimo_tts_voice.clone(),
                format: config.mimo_tts_format.clone(),
            };

            let provider =
                MimoTtsProvider::new(tts_config, call_uuid.to_string(), on_audio, on_turn_end);
            Ok(Arc::new(provider))
        }
        // Empty string or "volcano" → Volcano (default TTS backend)
        "" | "volcano" => {
            if config.volcano_api_key.is_empty() {
                anyhow::bail!("Volcano API key is not configured");
            }

            let endpoint = if config.volcano_tts_url.is_empty() {
                "wss://openspeech.bytedance.com/api/v3/tts/bidirection".to_string()
            } else {
                config.volcano_tts_url.clone()
            };
            let provider = VolcanoTtsProvider::new(
                endpoint,
                config.volcano_api_key.clone(),
                config.volcano_resource_id.clone(),
                config.volcano_speaker.clone(),
                call_uuid.to_string(),
                on_audio,
                on_turn_end,
            );
            Ok(Arc::new(provider))
        }
        _ => {
            anyhow::bail!(
                "Unknown TTS provider: {}. Supported: volcano (default), mimo",
                config.tts_provider
            );
        }
    }
}
