//! AI pipeline (audio-native LLM + Volcano TTS) — free functions driven by
//! [`crate::actor::CallActor`].
//!
//! "AudioLlm" mode (no ASR): caller audio → WAV → base64 → multimodal LLM
//! message → `speak(text)` tool → Volcano TTS → 16 kHz PCM → SPSC ringbuf
//! (`on_audio` → `Producer`; `read_frame` → `Consumer`) → caller.
//!
//! [`turn_pipeline`] runs as a background task spawned by the CallActor's
//! `StreamMessage<Vec<i16>>` handler (fed by `attach_stream` on the per-call
//! speech-segment channel); it is fully parameterized (no `&self`) so the actor
//! can hand off a `CancellationToken` + clones and stay responsive (mailbox not
//! blocked). On completion it sends a `TurnDone` message back to the actor to
//! persist conversation history.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use base64::Engine;
use tokio_util::sync::CancellationToken;

use kameo::actor::ActorRef;

use crate::actor::{CallActor, TurnDone};
use crate::audio_dsp::PIPELINE_SAMPLE_RATE;
use crate::call_core::CallControl;
use crate::tts::VolcanoBidirectionalSession;
use crate::voice_core::Config;

/// Maximum conversation history entries kept for LLM context.
pub const MAX_HISTORY: usize = 20;

/// A single message in the LLM conversation history (plain text; audio turns
/// attach the live WAV out-of-band — never persisted).
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

/// An LLM tool-call descriptor (OpenAI shape: function name + JSON arguments).
#[derive(Clone, Debug)]
pub struct ToolCall {
    pub name: String,
    pub arguments: String,
}

/// Result of executing one batch of LLM tool calls.
#[derive(Default, Debug)]
pub struct ToolExecutionResult {
    pub reply: String,
    pub asr: Option<String>,
    pub hangup: bool,
    pub dtmf: Option<String>,
    pub transfer: Option<String>,
}

/// Builds the full chat-completions URL from a configured base URL.
fn chat_completions_url(base: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    if trimmed.ends_with("/chat/completions") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/chat/completions")
    }
}

/// Run one speech segment through the full pipeline (LLM → speak tool → TTS).
///
/// Spawned as a background task by `CallActor::handle(StreamMessage::Next)`. Honors
/// `cancel` at each await point via `tokio::select!` so barge-in / actor-stop
/// interrupts the in-flight turn. On completion, sends `TurnDone` back to the
/// actor so it can persist the user/assistant turns into conversation history.
#[allow(clippy::too_many_arguments)]
pub async fn turn_pipeline(
    uuid: String,
    config: Option<Config>,
    conversation_snapshot: Vec<ChatMessage>,
    tts_session: Option<VolcanoBidirectionalSession>,
    audio: Vec<i16>,
    cancel: CancellationToken,
    ai_speaking: Arc<AtomicBool>,
    control: Arc<dyn CallControl>,
    actor_ref: ActorRef<CallActor>,
) {
    if audio.is_empty() {
        tracing::debug!("turn_pipeline {uuid}: empty speech segment, discarding");
        return;
    }

    let t0 = std::time::Instant::now();

    // ── Stage 1: perception (audio → multimodal LLM message) ────────────
    let wav = encode_wav(&audio, PIPELINE_SAMPLE_RATE);
    let b64 = base64::engine::general_purpose::STANDARD.encode(&wav);
    tracing::info!(
        "LATENCY {uuid}: stage1 encode_wav+b64 = {}ms ({} audio samples)",
        t0.elapsed().as_millis(),
        audio.len()
    );

    if cancel.is_cancelled() {
        tracing::info!("turn_pipeline {uuid}: cancelled before LLM call");
        return;
    }

    // ── Stage 2: LLM call with tools (cancellable) ──────────────────────
    let t1 = std::time::Instant::now();
    let (tool_calls, inline_text) = match tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            tracing::info!("turn_pipeline {uuid}: cancelled during LLM call");
            return;
        }
        r = call_llm_with_tools(config.as_ref(), &conversation_snapshot, &b64, &uuid) => r,
    } {
        Ok(pair) => pair,
        Err(e) => {
            tracing::error!("turn_pipeline {uuid}: LLM call failed: {e}");
            return;
        }
    };
    tracing::info!(
        "LATENCY {uuid}: stage2 LLM call = {}ms",
        t1.elapsed().as_millis()
    );

    if cancel.is_cancelled() {
        tracing::info!("turn_pipeline {uuid}: cancelled after LLM call");
        return;
    }

    // ── Stage 3: execute tool calls (speak → TTS, hangup/dtmf/transfer) ──
    let mut result = ToolExecutionResult::default();
    if !inline_text.is_empty() {
        result.reply = inline_text;
    }
    for tc in &tool_calls {
        match tc.name.as_str() {
            "speak" => {
                if let Some(text) = extract_string_arg(&tc.arguments, "text")
                    && !text.is_empty()
                {
                    tracing::info!(
                        "LLM speak: \"{}\" ({} chars)",
                        text.chars().take(80).collect::<String>(),
                        text.chars().count()
                    );
                    if !result.reply.is_empty() {
                        result.reply.push(' ');
                    }
                    result.reply.push_str(&text);
                }
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
            "hangup" => result.hangup = true,
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
            other => tracing::warn!("turn_pipeline {uuid}: unknown tool '{other}'"),
        }
    }

    let ToolExecutionResult {
        reply,
        asr,
        hangup,
        dtmf,
        transfer,
    } = result;
    tracing::info!(
        "LATENCY {uuid}: total before TTS = {}ms (LLM+tools)",
        t0.elapsed().as_millis()
    );

    // Synthesize the reply (if any) — cancellable.
    if !reply.is_empty()
        && !cancel.is_cancelled()
        && let Some(session) = tts_session.as_ref()
    {
        let t_tts = std::time::Instant::now();
        synthesize_and_play(session, &uuid, &reply, &cancel, &ai_speaking).await;
        tracing::info!(
            "LATENCY {uuid}: TTS synthesize (total) = {}ms",
            t_tts.elapsed().as_millis()
        );
    }

    // Call-control side effects. Under fire-and-forget, `synthesize` returned
    // before the audio played — let any in-flight TTS finish first so the
    // goodbye reaches the caller before media tears down (hangup/transfer) and
    // so DTMF doesn't mix with TTS audio. `wait_until_silent` is a no-op if
    // nothing is speaking.
    if hangup || dtmf.is_some() || transfer.is_some() {
        wait_until_silent(&ai_speaking, &cancel).await;
        if let Some(digits) = &dtmf
            && let Err(e) = control.send_dtmf(&uuid, digits)
        {
            tracing::warn!("turn_pipeline {uuid}: send_dtmf failed: {e}");
        }
        if let Some(dest) = &transfer
            && let Err(e) = control.transfer(&uuid, dest)
        {
            tracing::warn!("turn_pipeline {uuid}: transfer failed: {e}");
        }
        if hangup && let Err(e) = control.hangup(&uuid) {
            tracing::warn!("turn_pipeline {uuid}: hangup failed: {e}");
        }
    }

    // ── Stage 4: persist history via TurnDone message back to the actor ──
    let user_for_history = match &asr {
        Some(asr_text) if !asr_text.is_empty() => Some(ChatMessage::text("user", asr_text.clone())),
        _ => Some(ChatMessage::text("user", "[用户语音]".to_string())),
    };
    let assistant_for_history = if reply.is_empty() {
        None
    } else {
        Some(ChatMessage::text("assistant", reply))
    };
    tracing::info!(
        "turn complete for {uuid}: reply={} chars, asr={:?}",
        assistant_for_history
            .as_ref()
            .map(|m| m.content.chars().count())
            .unwrap_or(0),
        asr
    );
    let _ = actor_ref
        .tell(TurnDone {
            user: user_for_history,
            assistant: assistant_for_history,
        })
        .await;
}

/// Synthesize `text` via the Volcano session. Cancellable.
///
/// PCM is **not** forwarded here: the TTS driver loop (inside the Volcano
/// session) owns the ringbuf `Producer` via the `on_audio` callback installed
/// in [`crate::actor::init_call`], and pushes 8 kHz i16 PCM directly into the
/// SPSC ringbuf that `read_frame` drains.
///
/// Fire-and-forget: `synthesize` sends `task_request` and returns immediately
/// — it does NOT wait for the audio to play (call-lifetime Volcano session, one
/// `task_request` per turn; `finish_session` only at call end). `ai_speaking`
/// stays `true` here; the driver clears it via `on_turn_end` once the audio
/// stream goes idle. Media teardown (hangup/transfer) bridges the gap with
/// [`wait_until_silent`].
async fn synthesize_and_play(
    session: &VolcanoBidirectionalSession,
    uuid: &str,
    text: &str,
    cancel: &CancellationToken,
    ai_speaking: &AtomicBool,
) {
    ai_speaking.store(true, Ordering::Relaxed);

    let synth_cancel = cancel.clone();
    match session.synthesize(text, synth_cancel).await {
        Ok(true) => {
            tracing::info!("turn_pipeline {uuid}: TTS synthesize fired");
        }
        Ok(false) => {
            // Cancelled before the task_request was sent — no audio will play,
            // so no `on_turn_end` will fire. Clear `ai_speaking` ourselves.
            tracing::info!("turn_pipeline {uuid}: TTS synthesize cancelled before fire");
            ai_speaking.store(false, Ordering::Relaxed);
        }
        Err(e) => {
            tracing::error!("turn_pipeline {uuid}: TTS synthesize failed: {e}");
            // Clear on error so a later `wait_until_silent` doesn't hang waiting
            // for audio that will never arrive.
            ai_speaking.store(false, Ordering::Relaxed);
        }
    }
}

/// Wait for the current turn's TTS audio to finish playing before tearing down
/// media. Under fire-and-forget, [`synthesize`] returned before the audio
/// played; `ai_speaking` is cleared by the driver's `on_turn_end` on
/// stream-idle. No-op if nothing is speaking (e.g. empty reply, no session).
/// Bounded by 30s + `cancel` so a stuck driver can't hang the turn forever.
async fn wait_until_silent(ai_speaking: &AtomicBool, cancel: &CancellationToken) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        // Cancel is terminal — check it first so barge-in/drop doesn't wait a
        // full poll interval, and so `ai_speaking` stuck-true (no-audio stall)
        // doesn't pin this past the deadline pointlessly.
        if cancel.is_cancelled() || std::time::Instant::now() >= deadline {
            return;
        }
        if !ai_speaking.load(Ordering::Relaxed) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// Shared HTTP client for LLM calls. Reused across turns so the TLS
/// connector + connection pool (keep-alive to the same LLM endpoint) are
/// amortized instead of rebuilt per speech segment (~50-100ms setup each).
static LLM_CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
fn llm_client() -> &'static reqwest::Client {
    LLM_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

/// Call the LLM (OpenAI-compatible chat/completions) with conversation +
/// tool definitions + live audio as multimodal `input_audio`.
async fn call_llm_with_tools(
    config: Option<&Config>,
    messages: &[ChatMessage],
    live_audio_b64: &str,
    uuid: &str,
) -> Result<(Vec<ToolCall>, String)> {
    let Some(cfg) = config else {
        return Ok(canned_response(uuid, messages));
    };
    if cfg.api.llm_url.is_empty() || cfg.api.llm_key.is_empty() {
        return Ok(canned_response(uuid, messages));
    }

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
    tracing::debug!("LLM request body ({uuid}): {body}");

    let url = chat_completions_url(&cfg.api.llm_url);
    let resp = llm_client()
        .post(&url)
        .bearer_auth(&cfg.api.llm_key)
        .json(&body)
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

/// Canned fallback when no LLM backend is configured / the HTTP call fails.
fn canned_response(uuid: &str, _messages: &[ChatMessage]) -> (Vec<ToolCall>, String) {
    tracing::info!("turn_pipeline {uuid}: using canned LLM response");
    let reply = "这是占位回复。".to_string();
    let tc = ToolCall {
        name: "speak".to_string(),
        arguments: serde_json::json!({ "text": reply }).to_string(),
    };
    (vec![tc], String::new())
}

// ── Free helpers ───────────────────────────────────────────────────────

fn encode_wav(audio: &[i16], sample_rate: u32) -> Vec<u8> {
    let mut wav = Vec::with_capacity(44 + audio.len() * 2);
    let data_size = audio.len() as u32 * 2;
    let file_size = 36 + data_size;
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&file_size.to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_size.to_le_bytes());
    for &s in audio {
        wav.extend_from_slice(&s.to_le_bytes());
    }
    wav
}

fn tool_definitions() -> serde_json::Value {
    serde_json::json!([
        { "type": "function", "function": {
            "name": "speak",
            "description": "Speak the given text to the caller. Use this for ALL verbal replies.",
            "parameters": { "type": "object", "properties": {
                "text": { "type": "string", "description": "The text to synthesize and speak." },
                "asr":  { "type": "string", "description": "Optional: your transcript of what the user said." }
            }, "required": ["text"] }
        } },
        { "type": "function", "function": {
            "name": "hangup", "description": "Hang up the call.",
            "parameters": { "type": "object", "properties": {} }
        } },
        { "type": "function", "function": {
            "name": "send_dtmf", "description": "Send DTMF digits on the call.",
            "parameters": { "type": "object", "properties": {
                "digits": { "type": "string", "description": "DTMF digits (0-9, *, #)." }
            }, "required": ["digits"] }
        } },
        { "type": "function", "function": {
            "name": "transfer", "description": "Transfer the call to a new destination.",
            "parameters": { "type": "object", "properties": {
                "destination": { "type": "string", "description": "Transfer destination." }
            }, "required": ["destination"] }
        } }
    ])
}

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

fn extract_string_arg(arguments_json: &str, field: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(arguments_json).ok()?;
    v.get(field)?.as_str().map(|s| s.to_string())
}
