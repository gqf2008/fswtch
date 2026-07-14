//! Doubao Responses API client (raw HTTP, no rig dependency).
//!
//! Uses the `/responses` endpoint (not `/chat/completions`) because Doubao's
//! Responses API is ~2-3s faster for audio-native multimodal turns.
//!
//! Streaming events:
//!  - `response.output_text.delta` → text delta (for TTS dispatch)
//!  - `response.function_call_arguments.delta` → incremental tool call args
//!  - `response.output_item.done` → complete tool call
//!  - `response.completed` → stream end

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use base64::Engine;
use tokio_util::sync::CancellationToken;

use crate::audio_dsp::PIPELINE_SAMPLE_RATE;
use crate::orchestrator::ChatMessage;

/// Shared HTTP client for LLM calls.
static LLM_CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
fn llm_client() -> &'static reqwest::Client {
    LLM_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

/// A Doubao Responses API LLM provider.
///
/// Uses `/responses` (not `/chat/completions`) for lower latency on audio input.
/// Constructed from `ApiConfig` (base_url, key, model).
#[derive(Clone)]
pub struct DoubaoResponsesLlm {
    base_url: String,
    api_key: String,
    model: String,
    temperature: Option<f64>,
    max_tokens: Option<u64>,
}

impl DoubaoResponsesLlm {
    pub fn new(
        base_url: String,
        api_key: String,
        model: String,
        temperature: Option<f64>,
        max_tokens: Option<u64>,
    ) -> Self {
        Self {
            base_url,
            api_key,
            model,
            temperature,
            max_tokens,
        }
    }

    /// Send a streaming request to the Responses API.
    ///
    /// Calls `on_text_delta(&str)` for each text delta, and collects tool calls.
    /// Returns `(tool_calls, full_reply)`.
    pub async fn stream_with_tools(
        &self,
        messages: &[ChatMessage],
        system_prompt: Option<&str>,
        live_audio_b64: &str,
        transcribed_text: Option<&str>,
        cancel: &CancellationToken,
        on_text_delta: &mut (dyn FnMut(&str) + Send),
    ) -> Result<(Vec<crate::orchestrator::ToolCall>, String)> {
        use futures::StreamExt;

        let url = format!("{}/responses", self.base_url.trim_end_matches('/'));

        // Build input items: prior conversation + current user turn.
        let mut input: Vec<serde_json::Value> = Vec::new();
        for msg in messages {
            if msg.role == "system" {
                continue;
            } // system goes to `instructions`
            input.push(serde_json::json!({
                "type": "message",
                "role": msg.role,
                "content": [{"type": "input_text", "text": msg.content}],
            }));
        }
        // Current user turn: audio or text.
        if let Some(text) = transcribed_text {
            input.push(serde_json::json!({
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": text}],
            }));
        } else {
            let data_uri = format!("data:audio/wav;base64,{live_audio_b64}");
            input.push(serde_json::json!({
                "type": "message",
                "role": "user",
                "content": [
                    {"type": "input_audio", "audio_url": data_uri},
                ],
            }));
        }

        let mut body = serde_json::json!({
            "model": self.model,
            "input": input,
            "tools": tool_definitions_flat(),
            "tool_choice": "required",
            "reasoning": {"effort": "minimal"},
            "stream": true,
        });
        if let Some(sp) = system_prompt {
            body["instructions"] = serde_json::json!(sp);
        }
        if let Some(t) = self.temperature {
            body["temperature"] = serde_json::json!(t);
        }
        if let Some(m) = self.max_tokens {
            body["max_output_tokens"] = serde_json::json!(m);
        }

        let send_result = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok((Vec::new(), String::new())),
            r = llm_client().post(&url)
                .header("Authorization", format!("Bearer {}", self.api_key))
                .json(&body)
                .send() => r,
        };
        let resp = send_result.context("Doubao Responses HTTP request failed")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Doubao Responses HTTP {status}: {text}");
        }

        // Stream SSE events.
        let mut stream = resp.bytes_stream();
        let mut buffer = String::new();
        let mut full_reply = String::new();
        let mut tool_calls: Vec<crate::orchestrator::ToolCall> = Vec::new();
        // Track pending function call for incremental args.
        let mut pending_name: Option<String> = None;
        let mut pending_args = String::new();
        let mut forwarded_text_len: usize = 0;
        let t0 = std::time::Instant::now();
        let mut first_token_seen = false;

        loop {
            let chunk = tokio::select! {
                biased;
                _ = cancel.cancelled() => break,
                c = stream.next() => match c {
                    Some(c) => c,
                    None => break,
                },
            };
            let bytes = chunk?;
            buffer.push_str(std::str::from_utf8(&bytes).unwrap_or(""));
            if buffer.contains('\r') {
                buffer.retain(|c| c != '\r');
            }
            while let Some(pos) = buffer.find("\n\n") {
                let event_block: String = buffer.drain(..pos).collect();
                buffer.drain(..2);
                // Extract event type from the "event:" line + data from "data:" line.
                let mut event_type = String::new();
                let mut data = String::new();
                for raw in event_block.lines() {
                    let line = raw.trim();
                    if line.is_empty() || line.starts_with(':') {
                        continue;
                    }
                    if let Some(e) = line.strip_prefix("event: ") {
                        event_type = e.to_string();
                    } else if let Some(d) = line.strip_prefix("data: ") {
                        if !data.is_empty() {
                            data.push('\n');
                        }
                        data.push_str(d);
                    } else if line.starts_with("data:") {
                        // Some SSE streams don't have space after "data:"
                        if !data.is_empty() {
                            data.push('\n');
                        }
                        data.push_str(&line[5..]);
                    }
                }
                if data.is_empty() {
                    continue;
                }
                let parsed: serde_json::Value = match serde_json::from_str(&data) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                match event_type.as_str() {
                    "response.output_text.delta" => {
                        if let Some(delta) = parsed.get("delta").and_then(|d| d.as_str()) {
                            if !first_token_seen {
                                first_token_seen = true;
                                tracing::info!(
                                    "LATENCY Doubao Responses: first text delta = {}ms",
                                    t0.elapsed().as_millis()
                                );
                            }
                            full_reply.push_str(delta);
                            on_text_delta(delta);
                        }
                    }
                    "response.output_item.added" => {
                        // A new function call started — record its name.
                        if let Some(item) = parsed.get("item") {
                            if item.get("type").and_then(|t| t.as_str()) == Some("function_call") {
                                pending_name = item
                                    .get("name")
                                    .and_then(|n| n.as_str())
                                    .map(|s| s.to_string());
                                pending_args.clear();
                                forwarded_text_len = 0;
                                if !first_token_seen {
                                    first_token_seen = true;
                                    tracing::info!(
                                        "LATENCY Doubao Responses: first tool call = {}ms",
                                        t0.elapsed().as_millis()
                                    );
                                }
                            }
                        }
                    }
                    "response.function_call_arguments.delta" => {
                        if let Some(delta) = parsed.get("delta").and_then(|d| d.as_str()) {
                            pending_args.push_str(delta);
                            // Incremental speak-tool text extraction:
                            // if this is a "speak" call, extract the "text"
                            // field from the partial JSON and forward the
                            // new suffix to TTS immediately.
                            if pending_name.as_deref() == Some("speak") {
                                if let Some(text_so_far) =
                                    extract_partial_json_string(&pending_args, "text")
                                {
                                    if text_so_far.len() > forwarded_text_len {
                                        let new_text =
                                            text_so_far[forwarded_text_len..].to_string();
                                        forwarded_text_len = text_so_far.len();
                                        if !first_token_seen {
                                            first_token_seen = true;
                                            tracing::info!(
                                                "LATENCY Doubao Responses: first speak delta = {}ms",
                                                t0.elapsed().as_millis()
                                            );
                                        }
                                        full_reply.push_str(&new_text);
                                        on_text_delta(&new_text);
                                    }
                                }
                            }
                        }
                    }
                    "response.output_item.done" => {
                        // Complete tool call — use authoritative values.
                        if let Some(item) = parsed.get("item") {
                            if item.get("type").and_then(|t| t.as_str()) == Some("function_call") {
                                let name = item.get("name").and_then(|n| n.as_str()).unwrap_or("");
                                let args = item
                                    .get("arguments")
                                    .and_then(|a| a.as_str())
                                    .unwrap_or(&pending_args);
                                tracing::info!("Doubao Responses tool call: {name} ({args})");
                                tool_calls.push(crate::orchestrator::ToolCall {
                                    name: name.to_string(),
                                    arguments: args.to_string(),
                                });
                            }
                        }
                        pending_name = None;
                        pending_args.clear();
                        forwarded_text_len = 0;
                    }
                    "response.completed" => { /* stream done */ }
                    _ => {}
                }
            }
        }

        Ok((tool_calls, full_reply))
    }
}

/// Tool definitions in Doubao Responses API flat format
/// ({type, name, description, parameters} — NOT nested like OpenAI).
fn tool_definitions_flat() -> serde_json::Value {
    serde_json::json!([
        {"type": "function", "name": "speak",
         "description": "Speak the given text to the caller. Use this for ALL verbal replies. Set hangup=true to end the call after speaking. Use hangup_delay to specify how many seconds to wait after speaking before hanging up (e.g. short text=3, long text=8).",
         "parameters": {"type": "object", "properties": {
             "text": {"type": "string", "description": "The text to synthesize and speak."},
             "asr": {"type": "string", "description": "Optional: your transcript of what the user said."},
             "hangup": {"type": "boolean", "description": "If true, hang up the call after speaking. Default false."},
             "hangup_delay": {"type": "number", "description": "REQUIRED when hangup=true. Seconds to wait after speaking before hanging up. Max 15. Estimate: 0.15s per Chinese character, e.g. 10 chars=1.5s, 30 chars=4.5s, 50 chars=7.5s."}
         }, "required": ["text"]}},
        {"type": "function", "name": "hangup",
         "description": "Hang up the call.",
         "parameters": {"type": "object", "properties": {}}},
        {"type": "function", "name": "send_dtmf",
         "description": "Send DTMF digits on the call.",
         "parameters": {"type": "object", "properties": {
             "digits": {"type": "string", "description": "DTMF digits (0-9, *, #)."}
         }, "required": ["digits"]}},
        {"type": "function", "name": "transfer",
         "description": "Transfer the call to a new destination.",
         "parameters": {"type": "object", "properties": {
             "destination": {"type": "string", "description": "Transfer destination."}
         }, "required": ["destination"]}},
    ])
}

/// Extract the value of a string field from partial (incomplete) JSON.
/// Used to incrementally forward the `text` field of a `speak` tool call
/// to TTS before the full JSON arguments are complete.
fn extract_partial_json_string(json: &str, field: &str) -> Option<String> {
    let key = format!("\"{field}\"");
    let key_pos = json.find(&key)?;
    let after_key = &json[key_pos + key.len()..];
    let colon_pos = after_key.find(':')?;
    let after_colon = after_key[colon_pos + 1..].trim_start();

    if !after_colon.starts_with('"') {
        return None;
    }
    let value_start = &after_colon[1..]; // skip opening quote

    let mut end = 0;
    let mut chars = value_start.char_indices();
    while let Some((i, ch)) = chars.next() {
        if ch == '\\' {
            chars.next();
            end = i + 2;
            continue;
        }
        if ch == '"' {
            return Some(value_start[..i].to_string());
        }
        end = i + ch.len_utf8();
    }
    // No closing quote yet — return what we have so far.
    if end > 0 {
        Some(value_start[..end].to_string())
    } else {
        Some(String::new())
    }
}

/// Implement `LlmProvider` for `DoubaoResponsesLlm` so it can be used as a
/// trait object in the orchestrator.
impl crate::providers::LlmProvider for DoubaoResponsesLlm {
    fn completion(
        &self,
        messages: Vec<serde_json::Value>,
        audio_b64: &str,
        _tools: Option<&serde_json::Value>,
        uuid: &str,
    ) -> Pin<
        Box<dyn Future<Output = Result<(Vec<crate::orchestrator::ToolCall>, String)>> + Send + '_>,
    > {
        let chat_messages: Vec<crate::orchestrator::ChatMessage> = messages
            .iter()
            .filter_map(|m| {
                let role = m.get("role")?.as_str()?.to_string();
                let content = m.get("content")?.as_str()?.to_string();
                Some(crate::orchestrator::ChatMessage { role, content })
            })
            .collect();
        let audio_b64 = audio_b64.to_string();
        let uuid = uuid.to_string();

        Box::pin(async move {
            let result = self
                .stream_with_tools(
                    &chat_messages,
                    None,
                    &audio_b64,
                    None,
                    &tokio_util::sync::CancellationToken::new(),
                    &mut |_| {},
                )
                .await?;
            Ok(result)
        })
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
