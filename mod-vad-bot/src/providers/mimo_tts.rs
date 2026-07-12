//! MIMO TTS provider implementation.
//!
//! Wraps `MimoTtsHandle` to conform to our dyn-compatible `TtsProvider` trait.
//! MIMO TTS is fire-and-forget (like Volcano): `MimoTtsHandle::synthesize`
//! pushes audio through `on_audio` → ringbuf, and fires `on_turn_end` on
//! completion. `synthesize` here returns empty `Vec<u8>`.

use crate::audio_dsp::OnAudio;
use crate::mimo_tts::MimoTtsHandle;
use crate::providers::TtsProvider;
use crate::tts::OnTurnEnd;
use anyhow::Result;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tracing::debug;

/// MIMO TTS provider configuration.
#[derive(Clone)]
pub struct MimoTtsConfig {
    pub api_key: String,
    pub base_url: String,
    pub voice: String,
    pub format: String,
}

/// MIMO TTS provider using HTTP REST API.
pub struct MimoTtsProvider {
    handle: Arc<MimoTtsHandle>,
}

impl MimoTtsProvider {
    pub fn new(
        config: MimoTtsConfig,
        call_uuid: String,
        on_audio: OnAudio,
        on_turn_end: OnTurnEnd,
    ) -> Self {
        let handle = MimoTtsHandle::new(
            config.api_key,
            config.base_url,
            config.voice,
            config.format,
            call_uuid,
            on_audio,
            on_turn_end,
        );
        Self {
            handle: Arc::new(handle),
        }
    }
}

impl TtsProvider for MimoTtsProvider {
    fn synthesize(&self, text: &str) -> Pin<Box<dyn Future<Output = Result<Vec<u8>>> + Send + '_>> {
        let text = text.to_string();
        let handle = self.handle.clone();

        Box::pin(async move {
            let cancel = tokio_util::sync::CancellationToken::new();
            match handle.synthesize(&text, cancel).await {
                Ok(true) => {
                    // Audio was pushed via on_audio → ringbuf (fire-and-forget).
                    debug!("MIMO TTS synthesize fired");
                    Ok(Vec::new())
                }
                Ok(false) => {
                    debug!("MIMO TTS synthesize cancelled");
                    Ok(Vec::new())
                }
                Err(e) => Err(anyhow::anyhow!("MIMO TTS synthesize failed: {}", e)),
            }
        })
    }

    fn cancel(&self) {
        self.handle.cancel();
    }
}
