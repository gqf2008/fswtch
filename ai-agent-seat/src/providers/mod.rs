//! Provider abstraction layer for LLM and TTS backends.
//!
//! This module provides factory functions to build provider-agnostic LLM and TTS
//! models. We define our own dyn-compatible traits for runtime polymorphism
//! since rig's traits use `impl Future` and are not object-safe.

pub mod factory;
pub mod mimo_tts;
pub mod volcano_tts;

use anyhow::Result;
use std::future::Future;
use std::pin::Pin;

/// Dyn-compatible TTS provider trait.
pub trait TtsProvider: Send + Sync {
    fn synthesize(&self, text: &str) -> Pin<Box<dyn Future<Output = Result<Vec<u8>>> + Send + '_>>;
    fn cancel(&self);
}

/// Dyn-compatible LLM provider trait.
pub trait LlmProvider: Send + Sync {
    /// Non-streaming completion → (tool_calls, text).
    fn completion(
        &self,
        messages: Vec<serde_json::Value>,
        audio_b64: &str,
        tools: Option<&serde_json::Value>,
        uuid: &str,
    ) -> Pin<
        Box<dyn Future<Output = Result<(Vec<crate::orchestrator::ToolCall>, String)>> + Send + '_>,
    >;

    /// Downcast to `Any` for type-specific access (e.g. `DoubaoResponsesLlm::stream_with_tools`).
    fn as_any(&self) -> &dyn std::any::Any;
}
