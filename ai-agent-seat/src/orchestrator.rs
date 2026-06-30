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
    turn_flags: crate::actor::TurnFlags,
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

    // ── Stage 2: LLM call ─────────────────────────────────────────────────
    // Two paths:
    //  - streaming (llm_stream enabled): SSE text stream → split into sentences
    //    → per-sentence fire-and-forget TTS (low first-audio latency). NO tools
    //    (tool-bearing replies need the non-streaming path; the streaming body
    //    omits `tools`, so the model replies with plain text).
    //  - non-streaming (default, or streaming disabled): `call_llm_with_tools`
    //    returns `(tool_calls, inline_text)`; tools are executed in Stage 3.
    let t1 = std::time::Instant::now();
    let use_stream_cfg = match config.as_ref() {
        Some(c) if c.api.llm_stream && tts_session.is_some() => Some(c),
        _ => None,
    };
    let (tool_calls, reply_from_llm): (Vec<ToolCall>, String) = match use_stream_cfg {
        Some(cfg) => {
            // Streaming text path — drives TTS directly; returns the full reply.
            match stream_llm_and_synthesize(
                cfg,
                &conversation_snapshot,
                &b64,
                &uuid,
                tts_session.as_ref(),
                &cancel,
                &turn_flags,
            )
            .await
            {
                Ok(reply) => {
                    tracing::info!(
                        "LATENCY {uuid}: stage2 LLM stream = {}ms ({} chars)",
                        t1.elapsed().as_millis(),
                        reply.chars().count()
                    );
                    (Vec::new(), reply) // no tools on the streaming path
                }
                Err(e) => {
                    tracing::error!("turn_pipeline {uuid}: LLM stream failed: {e}");
                    return;
                }
            }
        }
        _ => {
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
            (tool_calls, inline_text)
        }
    };

    if cancel.is_cancelled() {
        tracing::info!("turn_pipeline {uuid}: cancelled after LLM call");
        return;
    }

    // ── Stage 3: execute tool calls (speak → TTS, hangup/dtmf/transfer) ───
    // On the streaming path `tool_calls` is empty and TTS already fired; here
    // we only assemble the tool-driven reply (non-streaming path) + side effects.
    let mut result = ToolExecutionResult::default();
    if !reply_from_llm.is_empty() {
        result.reply = reply_from_llm;
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

    // Synthesize the reply (if any) — cancellable. On the streaming path TTS
    // already fired per-sentence inside `stream_llm_and_synthesize`, so skip the
    // whole-reply synthesis here (it would double-play).
    if use_stream_cfg.is_none()
        && !reply.is_empty()
        && !cancel.is_cancelled()
        && let Some(session) = tts_session.as_ref()
    {
        let t_tts = std::time::Instant::now();
        synthesize_and_play(session, &uuid, &reply, &cancel, &turn_flags).await;
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
        wait_until_silent(&turn_flags.turn_pending, &cancel).await;
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
/// Fire-and-forget: `synthesize_sentence` sends `task_request` and returns
/// immediately — it does NOT wait for the audio to play (call-lifetime Volcano
/// session, one `task_request` per turn; `finish_session` only at call end).
/// The turn flags stay `begin()`-ed here; the driver clears them via
/// `on_turn_end` once the audio stream goes idle. Media teardown (hangup/
/// transfer) bridges the gap with [`wait_until_silent`].
async fn synthesize_and_play(
    session: &VolcanoBidirectionalSession,
    uuid: &str,
    text: &str,
    cancel: &CancellationToken,
    turn_flags: &crate::actor::TurnFlags,
) {
    turn_flags.begin();

    let synth_cancel = cancel.clone();
    match session.synthesize_sentence(text, synth_cancel, false).await {
        Ok(true) => {
            tracing::info!("turn_pipeline {uuid}: TTS synthesize fired");
        }
        Ok(false) => {
            // Cancelled before the task_request was sent — no audio will play,
            // so no `on_turn_end` will fire. Clear the flags ourselves.
            tracing::info!("turn_pipeline {uuid}: TTS synthesize cancelled before fire");
            turn_flags.end();
        }
        Err(e) => {
            tracing::error!("turn_pipeline {uuid}: TTS synthesize failed: {e}");
            // Clear on error so a later `wait_until_silent` doesn't hang waiting
            // for audio that will never arrive.
            turn_flags.end();
        }
    }
}

/// Wait for the current turn's TTS audio to finish playing before tearing down
/// media. Under fire-and-forget, [`synthesize_sentence`] returned before the
/// audio played; `turn_pending` is cleared by the driver's `on_turn_end` on
/// stream-idle. No-op if nothing is pending (e.g. empty reply, no session).
/// Bounded by 30s + `cancel` so a stuck driver can't hang the turn forever.
async fn wait_until_silent(turn_pending: &AtomicBool, cancel: &CancellationToken) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        // Cancel is terminal — check it first so barge-in/drop doesn't wait a
        // full poll interval, and so `turn_pending` stuck-true (no-audio stall)
        // doesn't pin this past the deadline pointlessly.
        if cancel.is_cancelled() || std::time::Instant::now() >= deadline {
            return;
        }
        if !turn_pending.load(Ordering::Relaxed) {
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

/// Return the byte index just past the first sentence boundary in `text`, or
/// `None` if no boundary is found. Chinese `。！？` and `\n` are always
/// boundaries; Western `.!?` are boundaries only when followed by whitespace or
/// at end-of-string (so `3.14` is NOT split). Ported from voice-call
/// `find_sentence_boundary` (pure function).
fn find_sentence_boundary(text: &str) -> Option<usize> {
    // Iterate char indices directly (no Vec allocation). Called per LLM token
    // with the growing `sentence_buffer`, so avoiding the collect keeps it
    // allocation-free on the hot path.
    let mut chars = text.char_indices();
    while let Some((byte_idx, ch)) = chars.next() {
        match ch {
            '。' | '！' | '？' => return Some(byte_idx + ch.len_utf8()),
            '\n' => return Some(byte_idx + ch.len_utf8()),
            '.' | '!' | '?' => {
                let end = byte_idx + ch.len_utf8();
                match chars.clone().next() {
                    None => return Some(end), // end-of-string
                    Some((_, next_ch)) if matches!(next_ch, ' ' | '\n' | '\t' | '\r') => {
                        // Boundary — include the trailing whitespace.
                        let (nb, nch) = chars.next().unwrap();
                        return Some(nb + nch.len_utf8());
                    }
                    _ => {} // not followed by whitespace — not a boundary
                }
            }
            _ => {}
        }
    }
    None
}

/// Stream pure-text LLM reply tokens (SSE) and fire-and-forget each sentence's
/// TTS as it completes. Returns the full concatenated reply (for history).
///
/// Used only when `llm_stream` is enabled AND the turn has no tool calls
/// (tool-bearing replies go through the non-streaming path — tool semantics
/// need the complete response, and SSE tool-delta reassembly is out of scope).
/// Each sentence reuses the turn's single `ActiveTask` (`turn_open` after the
/// first) — the server's FIFO on the call-lifetime session + the single ringbuf
/// sink preserve audio order without a sequencer.
///
/// `turn_pending` + `tts_audio_active` are set true at the first sentence and
/// cleared by the driver's `on_turn_end` when the final stream goes idle.
async fn stream_llm_and_synthesize(
    config: &Config,
    messages: &[ChatMessage],
    live_audio_b64: &str,
    uuid: &str,
    tts_session: Option<&VolcanoBidirectionalSession>,
    cancel: &CancellationToken,
    turn_flags: &crate::actor::TurnFlags,
) -> Result<String> {
    use futures::StreamExt;

    // Build the streaming request: same multimodal messages, but text-only
    // (no tools — the non-streaming probe already decided tools weren't used)
    // and `stream: true`.
    let messages_json = build_llm_messages(messages, live_audio_b64);
    let mut body = serde_json::json!({
        "model": config.api.llm_model,
        "messages": messages_json,
        "stream": true,
    });
    apply_optional_body_fields(&mut body, config);

    let url = chat_completions_url(&config.api.llm_url);
    let send_result = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Ok(String::new()),
        r = llm_client()
            .post(&url)
            .bearer_auth(&config.api.llm_key)
            .json(&body)
            .send() => r,
    };
    let resp = send_result.context("LLM stream request failed")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("LLM stream HTTP {status}: {text}");
    }

    let mut stream = resp.bytes_stream();
    let mut buffer = String::new();
    let mut sentence_buffer = String::new();
    let mut full_reply = String::new();
    let mut turn_open = false;
    let t_first_token = std::time::Instant::now();
    let mut first_token_seen = false;

    loop {
        let chunk = tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            c = stream.next() => c,
        };
        let bytes = match chunk {
            Some(c) => c?,
            None => break,
        };
        buffer.push_str(std::str::from_utf8(&bytes).unwrap_or(""));
        // Normalize CRLF → LF in place (SSE spec uses `\r\n`; stripping `\r`
        // makes `\n\n` reliably delimit events). Cheaper than `replace` (no
        // full-buffer realloc).
        if buffer.contains('\r') {
            buffer.retain(|c| c != '\r');
        }
        // Process every complete SSE event (`\n\n`-delimited).
        while let Some(pos) = buffer.find("\n\n") {
            // Drain the event block + the `\n\n` separator in place (no copies).
            let event_block: String = buffer.drain(..pos).collect();
            buffer.drain(..2);
            // Concatenate `data:` lines (SSE spec).
            let mut assembled = String::new();
            for raw in event_block.lines() {
                let line = raw.trim();
                if line.is_empty() || line.starts_with(':') {
                    continue;
                }
                if let Some(d) = line.strip_prefix("data: ") {
                    if !assembled.is_empty() {
                        assembled.push('\n');
                    }
                    assembled.push_str(d);
                }
            }
            if assembled.is_empty() {
                continue;
            }
            if assembled == "[DONE]" {
                buffer.clear();
                break;
            }
            let parsed: serde_json::Value = match serde_json::from_str(&assembled) {
                Ok(v) => v,
                Err(_) => continue, // skip keep-alive / unparseable
            };
            let choices = match parsed["choices"].as_array() {
                Some(c) => c,
                None => continue,
            };
            for choice in choices {
                if let Some(content) = choice["delta"]["content"].as_str() {
                    if !first_token_seen {
                        first_token_seen = true;
                        tracing::info!(
                            "LATENCY {uuid}: LLM first token = {}ms",
                            t_first_token.elapsed().as_millis()
                        );
                    }
                    full_reply.push_str(content);
                    sentence_buffer.push_str(content);
                    // Dispatch every complete sentence as soon as it forms.
                    while let Some(boundary) = find_sentence_boundary(&sentence_buffer) {
                        // Split one sentence off the buffer in place (one alloc
                        // for the sentence, none for the remainder).
                        let sentence: String = sentence_buffer.drain(..boundary).collect();
                        if sentence.trim().is_empty() || cancel.is_cancelled() {
                            continue;
                        }
                        if let Some(session) = tts_session {
                            if !turn_open {
                                turn_flags.begin();
                            }
                            match session
                                .synthesize_sentence(&sentence, cancel.clone(), turn_open)
                                .await
                            {
                                Ok(true) => {
                                    turn_open = true;
                                }
                                Ok(false) => {
                                    // Cancelled before the task_request was sent —
                                    // no audio will play, so no `on_turn_end` will
                                    // fire. If this was the turn-opener, undo the
                                    // `begin()`; on a continuation sentence the
                                    // earlier sentence's task will retire via its
                                    // own idle timer, so leave it.
                                    if !turn_open {
                                        turn_flags.end();
                                    }
                                    // Stop dispatching — the turn is cancelled.
                                    break;
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "turn_pipeline {uuid}: sentence TTS send failed: {e}"
                                    );
                                    if !turn_open {
                                        // Turn-opener failed before any audio: undo
                                        // `begin()` (no on_turn_end will fire for a
                                        // task that was never installed / never sent).
                                        turn_flags.end();
                                    }
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Flush the trailing fragment (no terminal punctuation) as a final sentence.
    let tail = sentence_buffer.trim();
    if !tail.is_empty()
        && !cancel.is_cancelled()
        && let Some(session) = tts_session
    {
        if !turn_open {
            turn_flags.begin();
        }
        match session.synthesize_sentence(tail, cancel.clone(), turn_open).await {
            Ok(true) => {}
            // On cancel/error, mirror the in-loop handling: undo begin() if this
            // was the turn-opener (no on_turn_end will fire), then stop.
            Ok(false) | Err(_) => {
                if !turn_open {
                    turn_flags.end();
                }
            }
        }
    }

    // If the stream produced no text at all (and wasn't cancelled), the server
    // likely returned a non-SSE body (some endpoints ignore `stream:true` and
    // reply with a single JSON object). The SSE parser never found a `\n\n`
    // event boundary, so `full_reply` is empty. Surface this as an error so the
    // caller falls back / logs rather than silently producing no TTS.
    if full_reply.is_empty()
        && !cancel.is_cancelled()
        && !buffer.is_empty()
    {
        tracing::warn!(
            "LLM stream {uuid}: no SSE events parsed (non-SSE response? trailing buffer: {:?})",
            &buffer
        );
        anyhow::bail!("LLM stream produced no SSE events (non-SSE response?)");
    }

    Ok(full_reply)
}

/// Build the `messages` array: prior conversation + a multimodal user turn
/// carrying the live caller audio as WAV base64. Shared by the streaming and
/// non-streaming LLM paths so the multimodal message format stays in sync.
fn build_llm_messages(messages: &[ChatMessage], live_audio_b64: &str) -> Vec<serde_json::Value> {
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
    messages_json
}

/// Apply optional `temperature` + `max_tokens` fields from config to an LLM
/// request body (shared by the streaming and non-streaming paths).
fn apply_optional_body_fields(body: &mut serde_json::Value, cfg: &Config) {
    if let Some(t) = cfg.api.llm_temperature {
        body["temperature"] = serde_json::json!(t);
    }
    if let Some(m) = cfg.api.llm_max_tokens {
        body["max_tokens"] = serde_json::json!(m);
    }
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

    let messages_json = build_llm_messages(messages, live_audio_b64);

    let mut body = serde_json::json!({
        "model": cfg.api.llm_model,
        "messages": messages_json,
        "tools": tool_definitions(),
        "tool_choice": "auto",
    });
    apply_optional_body_fields(&mut body, cfg);
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

#[cfg(test)]
mod tests {
    use super::find_sentence_boundary;

    #[test]
    fn no_boundary_returns_none() {
        assert_eq!(find_sentence_boundary("正在思考"), None);
        assert_eq!(find_sentence_boundary("Hello world"), None);
        assert_eq!(find_sentence_boundary(""), None);
    }

    #[test]
    fn chinese_terminal_punct_is_boundary() {
        // Includes the punctuation in the fragment.
        assert_eq!(find_sentence_boundary("你好。"), Some("你好。".len()));
        assert_eq!(find_sentence_boundary("停！"), Some("停！".len()));
    }

    #[test]
    fn newline_is_boundary() {
        assert_eq!(find_sentence_boundary("第一行\n第二行"), Some("第一行\n".len()));
    }

    #[test]
    fn western_punct_only_with_trailing_whitespace() {
        // `.` at end-of-string is a boundary.
        assert_eq!(find_sentence_boundary("Hello."), Some(6));
        // `.` followed by space — boundary includes the trailing space.
        assert_eq!(find_sentence_boundary("First. Second."), Some(7));
    }

    #[test]
    fn decimal_number_is_not_split() {
        // `3.14` — the `.` is followed by `1`, not whitespace → no boundary.
        assert_eq!(find_sentence_boundary("3.14"), None);
    }

    #[test]
    fn first_boundary_wins() {
        // Multiple boundaries → returns the first.
        let s = "你好。世界！";
        let idx = find_sentence_boundary(s).unwrap();
        assert_eq!(&s[..idx], "你好。");
    }
}
