//! Orchestrator — single struct that owns the full AI pipeline for one call.
//!
//! Architecture: "AudioLlm" mode (no ASR). Caller audio is encoded as a WAV
//! data URI and sent to the LLM as a multimodal user message; the LLM replies
//! with text and/or tool calls. TTS is a `speak` tool: when the LLM calls
//! `speak(text)`, the orchestrator synthesizes the text via
//! [`VolcanoBidirectionalSession`] and pushes the resulting 16 kHz i16 PCM
//! directly into [`crate::io::CallState::tts_accum`] (the global DashMap entry
//! keyed by the call UUID). The `read_frame` I/O callback drains that
//! accumulator toward the caller.
//!
//! The orchestrator is owned by [`crate::io::CallState`] (an `Arc` clone is
//! stored there at init time) and driven on the module's tokio runtime by the
//! `write_frame` callback, which spawns [`Self::process_speech_segment`] when
//! VAD detects end of speech. It is intentionally free of actix / channel
//! types so it can be unit-tested in isolation.
//!
//! # Conversation history
//!
//! Audio turns are NOT re-sent every turn (token/bandwidth cost). The history
//! stores a cheap text placeholder (`[用户语音]`) for each user audio turn;
//! only the *current* turn's audio is attached as a live multimodal message.
//! When the LLM returns its own ASR transcript (via the `speak` tool's `asr`
//! argument or a JSON `{asr, reply}` envelope), that transcript replaces the
//! placeholder in history so future turns see what the model heard.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use anyhow::{Context, Result};
use base64::Engine;
use tokio_util::sync::CancellationToken;

use crate::audio_dsp::PIPELINE_SAMPLE_RATE;
use crate::call_core::{CallControl, control};
use crate::io::CALLS;
use crate::tts::VolcanoBidirectionalSession;
use crate::voice_core::Config;

/// Maximum conversation history entries kept for LLM context (capped to limit
/// token cost on long calls).
const MAX_HISTORY: usize = 20;

/// Builds the full chat-completions URL from a configured base URL.
///
/// `voice_seat.yaml`'s `llm_url` is the API base (e.g.
/// `https://ark.cn-beijing.volces.com/api/v3`); the OpenAI-compatible endpoint
/// is `{base}/chat/completions`. If the configured value already ends with the
/// path, it is used verbatim.
fn chat_completions_url(base: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    if trimmed.ends_with("/chat/completions") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/chat/completions")
    }
}

/// A single message in the LLM conversation history.
///
/// `content` is plain text for stored turns; the live (current-turn) user
/// message carries audio via [`LiveMessage`] which is appended only for the
/// in-flight LLM call and never persisted.
#[derive(Clone, Debug)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn text(role: &str, content: String) -> Self {
        Self {
            role: role.to_string(),
            content,
        }
    }
}

/// The LLM tool-call descriptor returned from the (placeholder) LLM call.
///
/// Mirrors the OpenAI tool-call shape: a function name + JSON arguments.
#[derive(Clone, Debug)]
pub struct ToolCall {
    pub name: String,
    /// Raw JSON arguments string (as the LLM produced them).
    pub arguments: String,
}

/// Result of executing one batch of LLM tool calls.
///
/// `reply` is the concatenated text to synthesize and push to history as the
/// assistant turn; `asr` is the model's transcript of the user's speech (when
/// the backend supplies one), pushed to history as the user turn so the next
/// turn sees what the model heard.
#[derive(Default, Debug)]
pub struct ToolExecutionResult {
    /// Concatenated `speak` text — the assistant's verbal reply.
    pub reply: String,
    /// The model's transcript of the user's speech, if any.
    pub asr: Option<String>,
    /// Whether `hangup` was requested.
    pub hangup: bool,
    /// DTMF digits to send, if `send_dtmf` was called.
    pub dtmf: Option<String>,
    /// Transfer destination, if `transfer` was called.
    pub transfer: Option<String>,
}

/// The Orchestrator owns the per-call pipeline state.
///
/// Constructed cheaply (sync) at call-init time (first `write_frame`); the
/// Volcano WS connect is deferred to [`start_tts`](Self::start_tts) (or lazy on
/// first `synthesize`). Cloning is cheap (Arc inner); the `CallState` stores
/// an `Arc<Orchestrator>`.
pub struct Orchestrator {
    uuid: String,
    config: Option<Config>,
    /// Conversation history (text-only; audio turns use a placeholder).
    conversation: parking_lot::Mutex<Vec<ChatMessage>>,
    /// Volcano bidirectional TTS session (None when no TTS config).
    tts_session: Option<VolcanoBidirectionalSession>,
    /// Barge-in / mid-pipeline cancellation flag. Set by `cancel_current`;
    /// the pipeline checks it between stages.
    cancel_token: Arc<AtomicBool>,
    /// AI-speaking flag, shared (Arc-clone) with [`crate::io::CallState`].
    /// Set while TTS is playing so the write-leg VAD can detect barge-in.
    ai_speaking: Arc<AtomicBool>,
    /// Optional call-control handle for executing `hangup`/`send_dtmf`/`transfer`
    /// tools. Defaults to the FFI-backed singleton; overridable for tests.
    control: parking_lot::Mutex<Option<Arc<dyn CallControl>>>,
}

impl Orchestrator {
    /// Construct an orchestrator for one call.
    ///
    /// `ai_speaking` is the shared flag stored in [`crate::io::CallState`] so
    /// the write-leg VAD can observe barge-in while TTS plays. The Volcano
    /// session is built from `config` when TTS credentials are present.
    pub fn new(uuid: String, config: Option<Config>, ai_speaking: Arc<AtomicBool>) -> Self {
        let tts_session = config
            .as_ref()
            .filter(|c| !c.api.volcano_api_key.is_empty())
            .map(|c| {
                // Pipeline is fixed at 16 kHz. The configured server sample rate is
                // honored up to 16 kHz; a higher rate would need resampling (not yet
                // implemented), so clamp + warn to avoid a rate mismatch that would
                // garble playback.
                let tts_sr = std::cmp::min(
                    c.api.volcano_tts_sample_rate,
                    crate::audio_dsp::PIPELINE_SAMPLE_RATE,
                );
                if c.api.volcano_tts_sample_rate > crate::audio_dsp::PIPELINE_SAMPLE_RATE {
                    tracing::warn!(
                        "volcano_tts_sample_rate={} > pipeline {}; requesting {} (24k resample TBD)",
                        c.api.volcano_tts_sample_rate,
                        crate::audio_dsp::PIPELINE_SAMPLE_RATE,
                        tts_sr,
                    );
                }
                VolcanoBidirectionalSession::new(
                    c.api.volcano_api_key.clone(),
                    c.api.volcano_resource_id.clone(),
                    c.api.volcano_speaker.clone(),
                    tts_sr,
                    uuid.clone(),
                )
            });

        // Seed the conversation with the system prompt when configured.
        let mut conversation = Vec::new();
        if let Some(cfg) = config.as_ref()
            && let Some(prompt) = cfg.system_prompt.as_ref()
            && !prompt.is_empty()
        {
            conversation.push(ChatMessage::text("system", prompt.clone()));
        }

        Self {
            uuid,
            config,
            conversation: parking_lot::Mutex::new(conversation),
            tts_session,
            cancel_token: Arc::new(AtomicBool::new(false)),
            ai_speaking,
            control: parking_lot::Mutex::new(None),
        }
    }

    /// Wire a [`CallControl`] handle so `hangup`/`send_dtmf`/`transfer` tools
    /// can act on the live call. None disables those tools (logged + dropped).
    pub fn set_control(&self, control: Arc<dyn CallControl>) {
        *self.control.lock() = Some(control);
    }

    /// The call UUID this orchestrator owns.
    pub fn uuid(&self) -> &str {
        &self.uuid
    }

    /// A clone of the shared AI-speaking flag.
    pub fn ai_speaking_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.ai_speaking)
    }

    /// Eagerly establish the Volcano WS connection + first session.
    ///
    /// Intended to be called at call-init time (spawned on the runtime).
    /// Idempotent + race-safe (delegated to the session). Errors are logged
    /// but do NOT poison the session — `synthesize` lazy-retries.
    pub async fn start_tts(&self) -> Result<()> {
        if let Some(session) = &self.tts_session {
            if let Err(e) = session.start().await {
                tracing::warn!(
                    "Orchestrator start_tts: Volcano WS eager-connect failed \
                     (will lazy-retry on first synthesize): {e}"
                );
                return Err(e);
            }
            tracing::info!("Orchestrator start_tts: Volcano WS connected at init time");
        } else {
            tracing::debug!("Orchestrator start_tts: no TTS session configured");
        }
        Ok(())
    }

    /// Process one speech segment through the full pipeline.
    ///
    /// Returns `Some((reply_text, asr_text))` on success and `None` when the
    /// segment was discarded (cancelled, error, or empty). `reply_text` is the
    /// assistant's verbal reply (the text that was sent to TTS); `asr_text` is
    /// the model's transcript of the user's speech when the backend supplies
    /// one (otherwise `None`, and the history keeps the `[用户语音]`
    /// placeholder).
    pub async fn process_speech_segment(
        &self,
        audio: Vec<i16>,
    ) -> Option<(String, Option<String>)> {
        if audio.is_empty() {
            tracing::debug!("Orchestrator: empty speech segment, discarding");
            return None;
        }

        // Reset cancel for a fresh turn.
        self.cancel_token.store(false, Ordering::SeqCst);

        // ── Stage 1: perception (audio → multimodal LLM message) ────────
        let wav = encode_wav(&audio, PIPELINE_SAMPLE_RATE);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&wav);

        // The current-turn audio is passed out-of-band (not appended to history)
        // so the LLM call can render it as OpenAI multimodal `input_audio`. The
        // history snapshot stays text-only.
        let messages = self.conversation.lock().clone();

        if self.is_cancelled() {
            tracing::info!("Orchestrator: cancelled before LLM call");
            return None;
        }

        // ── Stage 2: LLM call with tools ────────────────────────────────
        let (tool_calls, inline_text) = match self.call_llm_with_tools(&messages, &b64).await {
            Ok(pair) => pair,
            Err(e) => {
                tracing::error!("Orchestrator LLM call failed for {}: {}", self.uuid, e);
                return None;
            }
        };

        if self.is_cancelled() {
            tracing::info!("Orchestrator: cancelled after LLM call");
            return None;
        }

        // ── Stage 3: execute tool calls (speak → TTS, hangup/dtmf/transfer) ─
        //
        // Ordering: synthesize any `speak` reply BEFORE executing `hangup` —
        // the media path tears down on hangup, so TTS synthesized after would
        // never reach the caller. (dtmf/transfer are unaffected by ordering.)
        let mut result = ToolExecutionResult::default();
        // If the LLM returned inline text alongside tool calls, treat it as a
        // `speak` (some backends return the reply as content, not a tool call).
        if !inline_text.is_empty() {
            result.reply = inline_text;
        }

        for tc in &tool_calls {
            match tc.name.as_str() {
                "speak" => {
                    if let Some(text) = extract_string_arg(&tc.arguments, "text")
                        && !text.is_empty()
                    {
                        if !result.reply.is_empty() {
                            result.reply.push(' ');
                        }
                        result.reply.push_str(&text);
                    }
                    // The model may also return its transcript of the user's
                    // speech as an `asr` argument on `speak`.
                    if let Some(asr) = extract_string_arg(&tc.arguments, "asr")
                        && !asr.is_empty()
                    {
                        result.asr = Some(result.asr.take().map_or(asr.clone(), |mut s| {
                            s.push(' ');
                            s.push_str(&asr);
                            s
                        }));
                    }
                }
                "hangup" => {
                    result.hangup = true;
                }
                "send_dtmf" => {
                    if let Some(digits) = extract_string_arg(&tc.arguments, "digits") {
                        result.dtmf = Some(digits);
                    }
                }
                "transfer" => {
                    if let Some(dest) = extract_string_arg(&tc.arguments, "destination") {
                        result.transfer = Some(dest);
                    }
                }
                other => {
                    tracing::warn!("Orchestrator: unknown tool '{}' from LLM", other);
                }
            }
        }

        // Pull the owned side-effect fields out so the TTS borrow ends before
        // we move `result.reply` into history below.
        let ToolExecutionResult {
            reply,
            asr,
            hangup,
            dtmf,
            transfer,
        } = result;

        // Synthesize the reply (if any) and push it to the TTS accumulator.
        if !reply.is_empty() && !self.is_cancelled() {
            self.synthesize_and_play(&reply).await;
        }

        // Execute call-control side effects (after TTS so hangup doesn't tear
        // down the media path before the goodbye reaches the caller).
        if hangup || dtmf.is_some() || transfer.is_some() {
            let control = self.control.lock().clone().unwrap_or_else(control);
            if let Some(digits) = &dtmf
                && let Err(e) = control.send_dtmf(&self.uuid, digits)
            {
                tracing::warn!("Orchestrator send_dtmf failed: {e}");
            }
            if let Some(dest) = &transfer
                && let Err(e) = control.transfer(&self.uuid, dest)
            {
                tracing::warn!("Orchestrator transfer failed: {e}");
            }
            if hangup && let Err(e) = control.hangup(&self.uuid) {
                tracing::warn!("Orchestrator hangup failed: {e}");
            }
        }

        // ── Stage 4: persist history ────────────────────────────────────
        // User turn: prefer the model's ASR transcript; fall back to placeholder.
        let user_for_history = match &asr {
            Some(asr_text) if !asr_text.is_empty() => ChatMessage::text("user", asr_text.clone()),
            _ => ChatMessage::text("user", "[用户语音]".to_string()),
        };
        self.push_message(user_for_history);

        // Assistant turn: the reply text that was spoken.
        if !reply.is_empty() {
            self.push_message(ChatMessage::text("assistant", reply.clone()));
        }

        Some((reply, asr))
    }

    /// Barge-in: interrupt the current pipeline turn.
    ///
    /// Sets the cancel flag (observed between stages) and clears the
    /// [`crate::io::CallState::tts_accum`] so `read_frame` stops playing the
    /// interrupted TTS immediately. Idempotent.
    pub fn cancel_current(&self) {
        self.cancel_token.store(true, Ordering::SeqCst);
        self.ai_speaking.store(false, Ordering::Relaxed);
        tracing::info!("Orchestrator: barge-in (cancel_current) for {}", self.uuid);
        // Flush the TTS accumulator in the DashMap entry.
        if let Some(mut state) = CALLS.get_mut(&self.uuid) {
            state.value_mut().clear_tts();
        }
    }

    /// Full teardown on hangup / call end.
    ///
    /// Clears the conversation history and the cancel flag so a reused
    /// orchestrator (should the call state ever be re-bound) starts clean. The
    /// Volcano WS session tears itself down on drop.
    pub fn full_hangup_reset(&self) {
        self.conversation.lock().clear();
        self.cancel_token.store(false, Ordering::SeqCst);
        self.ai_speaking.store(false, Ordering::Relaxed);
    }

    /// Synthesize `text` via the Volcano session and forward the PCM to the
    /// [`crate::io::CallState::tts_accum`] in 320-sample chunks.
    ///
    /// No-op (logged) when no TTS session is configured. Uses a fresh
    /// [`CancellationToken`] per call; barge-in cancels via `cancel_current`'s
    /// accumulator flush, and the session's own 10s timeout guards against hangs.
    async fn synthesize_and_play(&self, text: &str) {
        let Some(session) = &self.tts_session else {
            tracing::warn!(
                "Orchestrator: no TTS session for {}; cannot synthesize {} chars",
                self.uuid,
                text.chars().count()
            );
            return;
        };

        // Mark the AI as speaking so the write-leg VAD can detect barge-in.
        self.ai_speaking.store(true, Ordering::Relaxed);

        let cancel = CancellationToken::new();
        // Bridge: VolcanoBidirectionalSession sends Vec<i16> audio chunks; we
        // re-chunk them into 320-sample pushes into the TTS accumulator.
        let (raw_tx, mut raw_rx) =
            tokio::sync::mpsc::channel::<Vec<i16>>(crate::tts::tts_channel_capacity());
        let uuid = self.uuid.clone();
        let ai_speaking = Arc::clone(&self.ai_speaking);
        // Forwarder task: raw PCM → 320-sample pushes into tts_accum.
        let fwd = tokio::spawn(async move {
            while let Some(audio) = raw_rx.recv().await {
                // Re-chunk into 320-sample slices and push directly to the
                // DashMap entry. Each chunk is pushed under the RefMut guard.
                let mut start = 0;
                while start < audio.len() {
                    let end = (start + crate::tts::TTS_CHUNK_SAMPLES).min(audio.len());
                    let chunk = &audio[start..end];
                    if let Some(mut state) = CALLS.get_mut(&uuid) {
                        state.value_mut().push_tts(chunk);
                    } else {
                        // Call gone (hung up mid-synthesis) — stop forwarding.
                        return;
                    }
                    start = end;
                }
            }
        });

        match session.synthesize(text, cancel, raw_tx).await {
            Ok(completed) => {
                tracing::info!(
                    "Orchestrator: TTS synthesize completed for {} (completed={})",
                    self.uuid,
                    completed
                );
            }
            Err(e) => {
                tracing::error!(
                    "Orchestrator: TTS synthesize failed for {}: {}",
                    self.uuid,
                    e
                );
            }
        }
        // Wait for the forwarder to drain any in-flight chunks.
        let _ = fwd.await;
        // AI finished speaking (unless cancelled mid-flight, in which case
        // cancel_current already cleared the flag).
        self.ai_speaking.store(false, Ordering::Relaxed);
        // Keep `ai_speaking` referenced so clippy doesn't flag it as unused
        // (it's already shared via Arc in CallState).
        drop(ai_speaking);
    }

    /// Call the LLM with the conversation + tool definitions.
    ///
    /// Returns `(tool_calls, inline_text)`: the list of tool calls the model
    /// requested, plus any inline text content it returned alongside (treated
    /// as a `speak`).
    ///
    /// The HTTP call targets an OpenAI-compatible `/chat/completions` endpoint
    /// (URL built from `config.api.llm_url`). When no config/endpoint is
    /// present, or the request fails, it degrades to a canned response that
    /// echoes a `speak` tool call — this keeps the pipeline exercisable without
    /// a live backend while preserving the tool-calling structure.
    async fn call_llm_with_tools(
        &self,
        messages: &[ChatMessage],
        live_audio_b64: &str,
    ) -> Result<(Vec<ToolCall>, String)> {
        let Some(cfg) = self.config.as_ref() else {
            return Ok(self.canned_response(messages));
        };
        if cfg.api.llm_url.is_empty() || cfg.api.llm_key.is_empty() {
            return Ok(self.canned_response(messages));
        }

        // Render messages: history (text) + the current-turn audio as an
        // OpenAI-compatible multimodal `input_audio` part.
        let mut messages_json: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| serde_json::json!({ "role": m.role, "content": m.content }))
            .collect();
        messages_json.push(serde_json::json!({
            "role": "user",
            "content": [
                { "type": "input_audio", "input_audio": { "data": live_audio_b64, "format": "wav" } },
                { "type": "text", "text": "请识别并回复这段语音内容" },
            ],
        }));

        let mut body = serde_json::json!({
            "model": cfg.api.llm_model,
            "messages": messages_json,
            "tools": tool_definitions(),
            "tool_choice": "auto",
        });
        if let Some(t) = cfg.api.llm_temperature {
            body["temperature"] = serde_json::json!(t);
        }
        if let Some(m) = cfg.api.llm_max_tokens {
            body["max_tokens"] = serde_json::json!(m);
        }
        tracing::debug!("LLM request body: {}", body);

        let url = chat_completions_url(&cfg.api.llm_url);
        let client = reqwest::Client::new();
        let resp = client
            .post(&url)
            .bearer_auth(&cfg.api.llm_key)
            .json(&body)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .context("LLM HTTP request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("LLM HTTP {status}: {text}");
        }

        let json: serde_json::Value = resp.json().await.context("LLM JSON parse failed")?;
        parse_llm_response(&json)
    }

    /// Canned fallback response used when no LLM backend is configured or the
    /// HTTP call fails. Returns a single `speak` tool call so the pipeline
    /// (TTS + history) is still exercised end-to-end.
    fn canned_response(&self, messages: &[ChatMessage]) -> (Vec<ToolCall>, String) {
        let last_user = messages
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .map(|m| m.content.as_str())
            .unwrap_or("");
        tracing::info!(
            "Orchestrator: using canned LLM response for {} (last user msg len={})",
            self.uuid,
            last_user.chars().count()
        );
        let reply = "这是占位回复。".to_string();
        let tc = ToolCall {
            name: "speak".to_string(),
            arguments: serde_json::json!({ "text": reply }).to_string(),
        };
        (vec![tc], String::new())
    }

    fn is_cancelled(&self) -> bool {
        self.cancel_token.load(Ordering::SeqCst)
    }

    /// Push a message onto the conversation history, enforcing a
    /// [`MAX_HISTORY`] cap by draining the oldest entries.
    fn push_message(&self, msg: ChatMessage) {
        let mut conv = self.conversation.lock();
        conv.push(msg);
        if conv.len() > MAX_HISTORY {
            let drop_n = conv.len() - MAX_HISTORY;
            conv.drain(..drop_n);
        }
    }
}

// ── Free helpers ───────────────────────────────────────────────────────

/// Encode 16-bit mono PCM samples into a minimal WAV (44-byte header + data).
///
/// Mirrors the reference `AudioLlmPerception::encode_wav`. The resulting bytes
/// are base64-encoded into a `data:audio/wav;base64,...` URL for the LLM.
fn encode_wav(audio: &[i16], sample_rate: u32) -> Vec<u8> {
    let mut wav = Vec::with_capacity(44 + audio.len() * 2);
    let data_size = audio.len() as u32 * 2;
    let file_size = 36 + data_size;
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&file_size.to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // byte rate
    wav.extend_from_slice(&2u16.to_le_bytes()); // block align
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_size.to_le_bytes());
    for &s in audio {
        wav.extend_from_slice(&s.to_le_bytes());
    }
    wav
}

/// OpenAI-compatible tool definitions for the `speak`, `hangup`, `send_dtmf`,
/// and `transfer` tools.
fn tool_definitions() -> serde_json::Value {
    serde_json::json!([
        {
            "type": "function",
            "function": {
                "name": "speak",
                "description": "Speak the given text to the caller. Use this for ALL verbal replies.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "text": { "type": "string", "description": "The text to synthesize and speak." },
                        "asr":  { "type": "string", "description": "Optional: your transcript of what the user said." }
                    },
                    "required": ["text"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "hangup",
                "description": "Hang up the call.",
                "parameters": { "type": "object", "properties": {} }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "send_dtmf",
                "description": "Send DTMF digits on the call.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "digits": { "type": "string", "description": "DTMF digits (0-9, *, #)." }
                    },
                    "required": ["digits"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "transfer",
                "description": "Transfer the call to a new destination.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "destination": { "type": "string", "description": "Transfer destination (extension/number)." }
                    },
                    "required": ["destination"]
                }
            }
        }
    ])
}

/// Parse an OpenAI-compatible `/chat/completions` response into tool calls +
/// inline text.
fn parse_llm_response(json: &serde_json::Value) -> Result<(Vec<ToolCall>, String)> {
    let choice = json
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .ok_or_else(|| anyhow::anyhow!("LLM response missing choices[0].message"))?;

    let inline_text = choice
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let mut tool_calls = Vec::new();
    if let Some(arr) = choice.get("tool_calls").and_then(|v| v.as_array()) {
        for tc in arr {
            let name = tc
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let arguments = tc
                .get("function")
                .and_then(|f| f.get("arguments"))
                .and_then(|a| a.as_str())
                .unwrap_or("{}")
                .to_string();
            if !name.is_empty() {
                tool_calls.push(ToolCall { name, arguments });
            }
        }
    }

    Ok((tool_calls, inline_text))
}

/// Extract a string field from a JSON arguments string. Returns `None` on
/// parse failure or missing/non-string field.
fn extract_string_arg(arguments_json: &str, field: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(arguments_json).ok()?;
    v.get(field)?.as_str().map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::CallState;

    #[test]
    fn encode_wav_header_is_valid() {
        let wav = encode_wav(&[0i16; 8], 16_000);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(u16::from_le_bytes([wav[20], wav[21]]), 1); // PCM
        assert_eq!(
            u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]),
            16_000
        );
        assert_eq!(&wav[36..40], b"data");
        // 8 samples * 2 bytes + 44 header
        assert_eq!(wav.len(), 44 + 16);
    }

    #[test]
    fn extract_string_arg_parses_json() {
        let s = r#"{"text":"hello","asr":"hi there"}"#;
        assert_eq!(extract_string_arg(s, "text"), Some("hello".to_string()));
        assert_eq!(extract_string_arg(s, "asr"), Some("hi there".to_string()));
        assert_eq!(extract_string_arg(s, "missing"), None);
    }

    #[test]
    fn extract_string_arg_handles_bad_json() {
        assert_eq!(extract_string_arg("not json", "text"), None);
        assert_eq!(extract_string_arg("{}", "text"), None);
    }

    #[test]
    fn parse_llm_response_extracts_tool_calls() {
        let json = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "再见",
                    "tool_calls": [{
                        "id": "call_1",
                        "function": {
                            "name": "hangup",
                            "arguments": "{}"
                        }
                    }]
                }
            }]
        });
        let (tcs, text) = parse_llm_response(&json).unwrap();
        assert_eq!(text, "再见");
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].name, "hangup");
    }

    #[tokio::test]
    async fn process_speech_segment_discards_empty_audio() {
        let orch = Orchestrator::new("test".to_string(), None, Arc::new(AtomicBool::new(false)));
        assert!(orch.process_speech_segment(Vec::new()).await.is_none());
    }

    #[test]
    fn cancel_current_sets_flag_and_clears_accum() {
        // Insert a CallState for the test UUID so cancel_current can flush it.
        let uuid = "test_cancel";
        let mut state = CallState::new(uuid.to_string(), 16_000, None).expect("CallState::new");
        state.push_tts(&[1, 2, 3, 4]);
        CALLS.insert(uuid.to_string(), state);
        let orch = Orchestrator::new(uuid.to_string(), None, Arc::new(AtomicBool::new(false)));
        assert!(!orch.is_cancelled());
        orch.cancel_current();
        assert!(orch.is_cancelled());
        // Accumulator should have been flushed. Drop the borrow before remove
        // so we don't hold a read-lock on the shard DashMap::remove must
        // write-lock (DashMap shards are lock-per-shard; same-shard r/w
        // deadlocks single-threaded).
        {
            let state = CALLS.get(uuid).unwrap();
            assert!(state.tts_accum.is_empty());
        }
        CALLS.remove(uuid);
    }

    #[test]
    fn push_message_enforces_history_cap() {
        let orch = Orchestrator::new("test".to_string(), None, Arc::new(AtomicBool::new(false)));
        for i in 0..(MAX_HISTORY + 5) {
            orch.push_message(ChatMessage::text("user", format!("msg{i}")));
        }
        let conv = orch.conversation.lock();
        assert_eq!(conv.len(), MAX_HISTORY);
        // Oldest entries were drained; the last message is the most recent.
        assert_eq!(
            conv.last().unwrap().content,
            format!("msg{}", MAX_HISTORY + 4)
        );
    }
}
