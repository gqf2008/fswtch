//! AI pipeline (audio-native LLM + TTS) — free functions driven by
//! [`crate::actor::CallActor`].
//!
//! "AudioLlm" mode (no ASR): caller audio → WAV → base64 → multimodal LLM
//! message → `speak(text)` tool → TTS → 16 kHz PCM → SPSC ringbuf
//! (`on_audio` → `Producer`; `read_frame` → `Consumer`) → caller.
//!
//! [`turn_pipeline`] runs as a background task spawned by the CallActor's
//! `StreamMessage<Vec<i16>>` handler (fed by `attach_stream` on the per-call
//! speech-segment channel); it is fully parameterized (no `&self`) so the actor
//! can hand off a `CancellationToken` + clones and stay responsive (mailbox not
//! blocked). On completion it sends a `TurnDone` message back to the actor to
//! persist conversation history.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use base64::Engine;
use tokio_util::sync::CancellationToken;

use kameo::actor::ActorRef;

use crate::actor::{CallActor, TurnDone};
use crate::audio_dsp::PIPELINE_SAMPLE_RATE;
use crate::call_core::CallControl;
use crate::providers::{LlmProvider, TtsProvider};
use crate::voice_core::Config;
use rig_core::providers::openai;

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
    /// Seconds to wait after TTS before hanging up (LLM-decided, max 15).
    pub hangup_delay: f64,
    pub dtmf: Option<String>,
    pub transfer: Option<String>,
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
    llm: Option<crate::doubao_responses::DoubaoResponsesLlm>,
    asr: Option<openai::TranscriptionModel>,
    tts: Option<Arc<dyn TtsProvider>>,
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

    // ── Stage 0: ASR (if pipeline_mode is "asr_llm_tts") ─────────────────
    let pipeline_mode = config
        .as_ref()
        .map(|c| c.api.pipeline_mode.as_str())
        .unwrap_or("audio_llm");
    let transcribed_text = if pipeline_mode == "asr_llm_tts" {
        if let Some(asr_model) = &asr {
            let t_asr = std::time::Instant::now();
            let wav = encode_wav(&audio, PIPELINE_SAMPLE_RATE);
            match transcribe_audio(asr_model, &wav, &uuid).await {
                Ok(text) => {
                    tracing::info!(
                        "LATENCY {uuid}: ASR = {}ms, transcribed: {}",
                        t_asr.elapsed().as_millis(),
                        text
                    );
                    Some(text)
                }
                Err(e) => {
                    tracing::error!("turn_pipeline {uuid}: ASR failed: {e}");
                    return;
                }
            }
        } else {
            tracing::warn!(
                "turn_pipeline {uuid}: pipeline_mode is asr_llm_tts but no ASR model configured"
            );
            return;
        }
    } else {
        None
    };

    // ── Stage 1: perception (audio → multimodal LLM message) ────────────
    // In ASR mode, audio was already encoded for transcription above; skip the
    // redundant re-encode since transcribed_text is set and the LLM paths
    // use text-only messages when transcribed_text is Some.
    let b64 = if transcribed_text.is_none() {
        let wav = encode_wav(&audio, PIPELINE_SAMPLE_RATE);
        base64::engine::general_purpose::STANDARD.encode(&wav)
    } else {
        String::new() // unused in ASR mode
    };
    tracing::info!(
        "LATENCY {uuid}: stage1 encode_wav+b64 = {}ms ({} audio samples)",
        t0.elapsed().as_millis(),
        audio.len()
    );

    if cancel.is_cancelled() {
        tracing::info!("turn_pipeline {uuid}: cancelled before LLM call");
        return;
    }

    // ── Stage 2: LLM call (Doubao Responses API, streaming + tools) ──────
    // Single unified streaming path: the Responses API streams text deltas
    // AND tool call argument deltas simultaneously. Text is dispatched to TTS
    // per-sentence as it arrives; speak tool call text is extracted
    // incrementally from the streaming JSON arguments. Tool calls are
    // collected for Stage 3 side-effects (hangup/dtmf/transfer).
    let t1 = std::time::Instant::now();
    let (tool_calls, reply_from_llm, tts_already_fired): (Vec<ToolCall>, String, bool) = if llm.is_none() {
        (Vec::new(), canned_response(&uuid, &conversation_snapshot).1, false)
    } else {
        let llm_provider = llm.as_ref().unwrap();

        let system_prompt = config.as_ref()
            .and_then(|c| c.system_prompt.as_ref().map(|s| s.as_str()));

        // Sentence-boundary splitter + TTS dispatch closure.
        let tts_ref = tts.as_ref().map(|t| t.clone());
        let flags_ref = turn_flags.clone();
        let cancel_ref = cancel.clone();
        let uuid_ref = uuid.clone();
        let sentence_buffer = Arc::new(std::sync::Mutex::new(String::new()));
        let turn_open = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let tts_fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let tts_fired_clone = Arc::clone(&tts_fired);

        let mut on_text_delta = move |delta: &str| {
            let mut buffer = sentence_buffer.lock().unwrap();
            buffer.push_str(delta);
            while let Some(boundary) = find_sentence_boundary(&buffer) {
                let sentence: String = buffer.drain(..boundary).collect();
                if sentence.trim().is_empty() || cancel_ref.is_cancelled() { continue; }
                if let Some(ref tts_provider) = tts_ref {
                    if !turn_open.load(std::sync::atomic::Ordering::Relaxed) {
                        flags_ref.begin();
                        turn_open.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    tts_fired.store(true, std::sync::atomic::Ordering::Relaxed);
                    // Fire-and-forget: spawn TTS so the LLM stream is not blocked.
                    let tts_clone = tts_provider.clone();
                    let sentence_clone = sentence;
                    let flags_clone = flags_ref.clone();
                    let uuid_clone = uuid_ref.clone();
                    crate::runtime::spawn(async move {
                        if let Err(e) = tts_clone.synthesize(&sentence_clone).await {
                            tracing::warn!("turn_pipeline {uuid_clone}: sentence TTS failed: {e}");
                            flags_clone.end();
                        }
                    });
                }
            }
        };

        let result = llm_provider.stream_with_tools(
            &conversation_snapshot,
            system_prompt,
            &b64,
            transcribed_text.as_deref(),
            &cancel,
            &mut on_text_delta,
        ).await;

        match result {
            Ok((tc, reply)) => {
                let fired = tts_fired_clone.load(std::sync::atomic::Ordering::Relaxed);
                tracing::info!(
                    "LATENCY {uuid}: stage2 LLM stream = {}ms ({} chars, {} tool calls, tts_fired={})",
                    t1.elapsed().as_millis(),
                    reply.chars().count(),
                    tc.len(),
                    fired,
                );
                (tc, reply, fired)
            }
            Err(e) => {
                tracing::error!("turn_pipeline {uuid}: LLM stream failed: {e}");
                return;
            }
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
                // speak tool can request hangup after speaking
                if extract_bool_arg(&tc.arguments, "hangup").unwrap_or(false) {
                    result.hangup = true;
                    // LLM must fill hangup_delay. If missing, estimate from text length.
                    let delay = extract_number_arg(&tc.arguments, "hangup_delay")
                        .unwrap_or_else(|| {
                            let chars = extract_string_arg(&tc.arguments, "text")
                                .map(|t| t.chars().count() as f64)
                                .unwrap_or(20.0);
                            tracing::warn!("turn_pipeline: hangup=true but no hangup_delay, estimating {chars} chars * 0.15s");
                            (chars * 0.15).max(1.0)
                        })
                        .min(15.0)
                        .max(0.5);
                    result.hangup_delay = delay;
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
        hangup_delay,
        dtmf,
        transfer,
    } = result;
    tracing::info!(
        "LATENCY {uuid}: total before TTS = {}ms (LLM+tools)",
        t0.elapsed().as_millis()
    );

    // Synthesize the reply (if any) — cancellable.
    // Skip if the streaming path already TTS'd the text (via on_text_delta
    // callback) — re-synthesizing would double-play.
    if !reply.is_empty() && !cancel.is_cancelled() && !tts_already_fired {
        if let Some(tts_provider) = tts.as_ref() {
            let t_tts = std::time::Instant::now();
            synthesize_and_play(tts_provider, &uuid, &reply, &cancel, &turn_flags).await;
            tracing::info!(
                "LATENCY {uuid}: TTS synthesize (total) = {}ms",
                t_tts.elapsed().as_millis()
            );
        }
    }

    // Do NOT wait_until_silent here — it blocks the actor mailbox, preventing
    // barge-in. Instead, the actor's StreamMessage::Next handler cancels TTS
    // + clears the ringbuf at the start of the next turn. This keeps the
    // mailbox free for BargeIn messages during TTS playback.

    // Call-control side effects. For hangup, wait the LLM-decided delay
    // (hangup_delay seconds, capped at 15) after TTS fires — gives audio
    // time to play before media tears down. Cancellable by barge-in.
    if hangup || dtmf.is_some() || transfer.is_some() {
        if hangup && hangup_delay > 0.0 {
            tracing::info!("turn_pipeline {uuid}: waiting {hangup_delay}s before hangup");
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {}
                _ = tokio::time::sleep(std::time::Duration::from_secs_f64(hangup_delay)) => {}
            }
        }
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

/// Synthesize `text` via the TTS provider. Blocks until the audio is ready.
/// Cancellable.
async fn synthesize_and_play(
    tts: &Arc<dyn TtsProvider>,
    uuid: &str,
    text: &str,
    cancel: &CancellationToken,
    turn_flags: &crate::actor::TurnFlags,
) {
    turn_flags.begin();

    match tts.synthesize(text).await {
        Ok(_audio_bytes) => {
            if cancel.is_cancelled() {
                tracing::info!("turn_pipeline {uuid}: TTS synthesize cancelled after fire");
                turn_flags.end();
                return;
            }
            // Fire-and-forget: TTS request is on the wire. Audio arrives
            // asynchronously via the driver's on_audio callback (→ ringbuf).
            // turn_flags are cleared by on_turn_end when the stream goes idle.
            tracing::info!("turn_pipeline {uuid}: TTS synthesize fired");
        }
        Err(e) => {
            tracing::error!("turn_pipeline {uuid}: TTS synthesize failed: {e}");
            // Clear on error so wait_until_silent doesn't hang.
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
/// Stream the LLM reply via rig's `CompletionModel::stream()` and fire TTS
/// per sentence as text deltas arrive. Collects any tool calls emitted by the
/// model (rig reassembles the streaming tool-call deltas into complete
/// `ToolCall` events for us).
///
/// Uses rig (not raw HTTP) so that:
///  - audio input serializes correctly (`input_audio`) via the OpenRouter
///    provider,
///  - streaming tool-call deltas are reassembled into complete `ToolCall`s,
///  - the `tools` field IS sent (unlike the old raw-HTTP path), so the model
///    can emit `speak`/`hangup`/`send_dtmf`/`transfer` on the streaming path.
///
/// Returns `(tool_calls, full_reply)`.
async fn stream_llm_and_synthesize(
    config: &Config,
    llm: &Option<Arc<dyn LlmProvider>>,
    messages: &[ChatMessage],
    live_audio_b64: &str,
    transcribed_text: Option<&str>,
    uuid: &str,
    tts: Option<&Arc<dyn TtsProvider>>,
    cancel: &CancellationToken,
    turn_flags: &crate::actor::TurnFlags,
) -> Result<(Vec<ToolCall>, String)> {
    let Some(provider) = llm.as_ref() else {
        return Ok(canned_response(uuid, messages));
    };

    // Use DoubaoResponsesLlm's stream_with_tools which handles SSE parsing,
    // tool call delta reassembly, and incremental speak text extraction.
    let doubao = provider.as_any().downcast_ref::<crate::doubao_responses::DoubaoResponsesLlm>();
    let Some(doubao) = doubao else {
        anyhow::bail!("streaming path requires DoubaoResponsesLlm provider");
    };

    let system_prompt = config.system_prompt.as_deref();
    let tts_ref = tts.cloned();
    let flags_ref = turn_flags.clone();
    let cancel_ref = cancel.clone();
    let uuid_ref = uuid.to_string();
    let sentence_buffer = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let turn_open = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let t0 = std::time::Instant::now();
    let mut first_token_seen = false;

    let sb = sentence_buffer.clone();
    let to = turn_open.clone();
    let result = doubao.stream_with_tools(
        messages,
        system_prompt,
        live_audio_b64,
        transcribed_text,
        cancel,
        &mut move |delta: &str| {
            let mut buf = sb.lock().unwrap();
            buf.push_str(delta);
            while let Some(boundary) = find_sentence_boundary(&buf) {
                let sentence: String = buf.drain(..boundary).collect();
                if sentence.trim().is_empty() || cancel_ref.is_cancelled() { continue; }
                if let Some(ref tts_provider) = tts_ref {
                    if !to.load(std::sync::atomic::Ordering::Relaxed) {
                        flags_ref.begin();
                        to.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    let tts_clone = tts_provider.clone();
                    let sentence_clone = sentence;
                    let flags_clone = flags_ref.clone();
                    let uuid_clone = uuid_ref.clone();
                    crate::runtime::spawn(async move {
                        if let Err(e) = tts_clone.synthesize(&sentence_clone).await {
                            tracing::warn!("turn_pipeline {uuid_clone}: sentence TTS failed: {e}");
                            flags_clone.end();
                        }
                    });
                }
            }
        },
    ).await;

    match result {
        Ok((tool_calls, reply)) => {
            tracing::info!(
                "LATENCY {uuid}: stage2 LLM stream = {}ms ({} chars, {} tool calls)",
                t0.elapsed().as_millis(),
                reply.chars().count(),
                tool_calls.len()
            );
            Ok((tool_calls, reply))
        }
        Err(e) => Err(e),
    }
}

/// Build rig `Message` vector from our `ChatMessage` history + the current
/// turn's user input (audio or transcribed text). Shared by the streaming and
/// non-streaming LLM paths so message format stays in sync.
fn build_rig_messages(
    messages: &[ChatMessage],
    live_audio_b64: &str,
    transcribed_text: Option<&str>,
) -> Result<Vec<rig_core::completion::Message>> {
    use rig_core::OneOrMany;
    use rig_core::completion::Message;
    use rig_core::message::{AssistantContent, UserContent};

    let mut rig_messages: Vec<Message> = messages
        .iter()
        .map(|m| {
            if m.role == "system" {
                Message::System {
                    content: m.content.clone(),
                }
            } else if m.role == "assistant" {
                Message::Assistant {
                    id: None,
                    content: OneOrMany::one(AssistantContent::text(&m.content)),
                }
            } else {
                Message::User {
                    content: OneOrMany::one(UserContent::text(&m.content)),
                }
            }
        })
        .collect();

    // Add the user input message (audio or text based on pipeline mode)
    if let Some(text) = transcribed_text {
        rig_messages.push(Message::User {
            content: OneOrMany::one(UserContent::text(text)),
        });
    } else {
        rig_messages.push(Message::User {
            content: OneOrMany::many(vec![
                UserContent::audio(live_audio_b64, Some(rig_core::message::AudioMediaType::WAV)),
                UserContent::text("请识别并回复这段语音内容"),
            ])
            .map_err(|e| anyhow::anyhow!("Failed to create audio message: {}", e))?,
        });
    }

    Ok(rig_messages)
}

/// Build a rig `CompletionRequest` from config + messages. Shared by the
/// streaming and non-streaming LLM paths.
fn build_completion_request(
    config: &Config,
    messages: &[ChatMessage],
    live_audio_b64: &str,
    transcribed_text: Option<&str>,
) -> Result<rig_core::completion::CompletionRequest> {
    use rig_core::OneOrMany;

    let rig_messages = build_rig_messages(messages, live_audio_b64, transcribed_text)?;
    Ok(rig_core::completion::CompletionRequest {
        model: None,
        preamble: None,
        chat_history: OneOrMany::many(rig_messages)
            .map_err(|e| anyhow::anyhow!("Failed to create chat history: {}", e))?,
        documents: vec![],
        tools: tool_definitions_rig(),
        temperature: config.api.llm_temperature.map(|t| t as f64),
        max_tokens: config.api.llm_max_tokens.map(|m| m as u64),
        tool_choice: None,
        additional_params: None,
        output_schema: None,
    })
}

/// Call the LLM (OpenAI-compatible chat/completions) with conversation +
/// tool definitions + live audio as multimodal `input_audio` OR transcribed text.
///
/// Uses rig's OpenRouter provider (not OpenAI's) because rig's OpenAI Chat
/// Completions provider serializes `UserContent::Audio` with `type:"audio"`,
/// which violates the OpenAI wire spec (`type:"input_audio"`) and is rejected
/// by Volcano (Doubao) with HTTP 400. The OpenRouter provider names the
/// variant `InputAudio`, which serializes correctly, and still targets
/// `/chat/completions`.
async fn call_llm_with_tools(
    llm: Option<&Arc<dyn LlmProvider>>,
    messages: &[ChatMessage],
    live_audio_b64: &str,
    uuid: &str,
) -> Result<(Vec<ToolCall>, String)> {
    let Some(provider) = llm else {
        return Ok(canned_response(uuid, messages));
    };

    // Convert ChatMessage to serde_json::Value for the LlmProvider trait.
    let messages_json: Vec<serde_json::Value> = messages
        .iter()
        .map(|m| serde_json::json!({"role": m.role, "content": m.content}))
        .collect();
    let result = provider.completion(messages_json, live_audio_b64, None, uuid).await?;
    Ok((result.0, result.1))
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

fn extract_string_arg(arguments_json: &str, field: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(arguments_json).ok()?;
    v.get(field)?.as_str().map(|s| s.to_string())
}

fn extract_bool_arg(arguments_json: &str, field: &str) -> Option<bool> {
    let v: serde_json::Value = serde_json::from_str(arguments_json).ok()?;
    match v.get(field)? {
        serde_json::Value::Bool(b) => Some(*b),
        serde_json::Value::String(s) => Some(matches!(s.to_lowercase().as_str(), "true" | "1" | "yes")),
        _ => None,
    }
}

fn extract_number_arg(arguments_json: &str, field: &str) -> Option<f64> {
    let v: serde_json::Value = serde_json::from_str(arguments_json).ok()?;
    match v.get(field)? {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// Extract the partial `"text"` value from an incomplete JSON arguments
/// string being built up across `ToolCallDelta` events.
///
/// This does NOT parse JSON. It scans the accumulated string for the first
/// occurrence of `"text"` (the key), then the `:` and opening `"` that follow,
/// and returns everything after that opening quote up to — but not including —
/// the closing quote (or, while the string is still incomplete, everything
/// available). Handles `\"` and `\\` escapes inside the value so a delta that
/// lands mid-escape doesn't corrupt extraction.
///
/// Examples:
///   `{"text":"你好世`        → `你好世`
///   `{"text":"hello","asr":` → `hello`
///   `{"text":"a\"b`          → `a"b`
///   `{"other":1,"text":"x`   → `x`
///   `{"text":"done"}`        → `done`
fn extract_partial_text_from_json(accumulated: &str) -> Option<String> {
    // Find the first `"text"` key. We search for the literal `"text"` (with
    // surrounding quotes) to avoid matching a substring of a longer key.
    let key = "\"text\"";
    let key_idx = accumulated.find(key)?;
    let after_key = &accumulated[key_idx + key.len()..];

    // Skip whitespace, then expect ':', then whitespace, then opening '"'.
    let mut chars = after_key.char_indices();
    let mut colon_seen = false;
    let mut quote_idx: Option<usize> = None;
    while let Some((i, ch)) = chars.next() {
        if !colon_seen {
            if ch.is_whitespace() {
                continue;
            }
            if ch == ':' {
                colon_seen = true;
                continue;
            }
            // Malformed — key not followed by ':'.
            return None;
        }
        // colon_seen
        if ch.is_whitespace() {
            continue;
        }
        if ch == '"' {
            // Opening quote of the value. `i` is the byte offset within
            // `after_key`; the value starts at i+1.
            quote_idx = Some(i + 1);
        }
        break;
    }
    let start = quote_idx?;
    let value = &after_key[start..];

    // Walk the value, stopping at the unescaped closing quote. While the
    // stream is incomplete there is no closing quote, so we return everything
    // accumulated so far.
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('/') => out.push('/'),
                Some('b') => out.push('\u{0008}'),
                Some('f') => out.push('\u{000C}'),
                Some('u') => {
                    // \uXXXX — best effort: take the 4 hex digits if present.
                    let mut hex = String::with_capacity(4);
                    for _ in 0..4 {
                        if let Some(h) = chars.next() {
                            hex.push(h);
                        }
                    }
                    if let Ok(code) = u32::from_str_radix(&hex, 16)
                        && let Some(c) = char::from_u32(code)
                    {
                        out.push(c);
                    }
                }
                Some(other) => {
                    // Unknown escape — keep the char literally.
                    out.push(other);
                }
                None => break, // delta ended mid-escape
            }
            continue;
        }
        if ch == '"' {
            // Unescaped closing quote — value complete.
            break;
        }
        out.push(ch);
    }
    Some(out)
}

/// Convert rig's tool definitions to our format
fn tool_definitions_rig() -> Vec<rig_core::completion::request::ToolDefinition> {
    use rig_core::completion::request::ToolDefinition;
    vec![
        ToolDefinition {
            name: "speak".to_string(),
            description: "Speak the given text to the caller. Use this for ALL verbal replies."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "The text to synthesize and speak."
                    },
                    "asr": {
                        "type": "string",
                        "description": "Optional: your transcript of what the user said."
                    }
                },
                "required": ["text"]
            }),
        },
        ToolDefinition {
            name: "hangup".to_string(),
            description: "Hang up the call.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        },
        ToolDefinition {
            name: "send_dtmf".to_string(),
            description: "Send DTMF digits on the call.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "digits": {
                        "type": "string",
                        "description": "DTMF digits (0-9, *, #)."
                    }
                },
                "required": ["digits"]
            }),
        },
        ToolDefinition {
            name: "transfer".to_string(),
            description: "Transfer the call to a new destination.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "destination": {
                        "type": "string",
                        "description": "Transfer destination."
                    }
                },
                "required": ["destination"]
            }),
        },
    ]
}

/// Parse rig's completion response into our format
fn parse_rig_response<T>(
    response: &rig_core::completion::CompletionResponse<T>,
) -> Result<(Vec<ToolCall>, String)> {
    use rig_core::completion::AssistantContent;

    let mut inline_text = String::new();
    let mut tool_calls = Vec::new();

    for content in response.choice.iter() {
        match content {
            AssistantContent::Text(text) => {
                inline_text.push_str(&text.text);
            }
            AssistantContent::ToolCall(tc) => {
                tool_calls.push(ToolCall {
                    name: tc.function.name.clone(),
                    arguments: tc.function.arguments.to_string(),
                });
            }
            _ => {}
        }
    }

    Ok((tool_calls, inline_text))
}

/// Transcribe audio using the ASR model
async fn transcribe_audio(
    asr_model: &openai::TranscriptionModel,
    wav_bytes: &[u8],
    uuid: &str,
) -> Result<String> {
    use rig_core::transcription::TranscriptionModel;

    let request = rig_core::transcription::TranscriptionRequest {
        data: wav_bytes.to_vec(),
        filename: "audio.wav".to_string(),
        language: Some("zh".to_string()),
        prompt: None,
        temperature: None,
        additional_params: None,
    };

    let response = asr_model
        .transcription(request)
        .await
        .context("ASR transcription failed")?;

    tracing::debug!("turn_pipeline {uuid}: ASR response: {}", response.text);
    Ok(response.text)
}

#[cfg(test)]
mod tests {
    use super::{extract_partial_text_from_json, find_sentence_boundary};

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
        assert_eq!(
            find_sentence_boundary("第一行\n第二行"),
            Some("第一行\n".len())
        );
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

    // ── extract_partial_text_from_json ──────────────────────────────────

    #[test]
    fn partial_text_incomplete_value() {
        // Stream cut mid-value (no closing quote yet).
        assert_eq!(
            extract_partial_text_from_json("{\"text\":\"你好世"),
            Some("你好世".to_string())
        );
    }

    #[test]
    fn partial_text_complete_value() {
        // Closing quote present → value complete.
        assert_eq!(
            extract_partial_text_from_json("{\"text\":\"hello\"}"),
            Some("hello".to_string())
        );
    }

    #[test]
    fn partial_text_with_trailing_fields() {
        // `text` followed by more fields — only `text`'s value is returned.
        assert_eq!(
            extract_partial_text_from_json("{\"text\":\"你好\",\"asr\":\"喂\"}"),
            Some("你好".to_string())
        );
    }

    #[test]
    fn partial_text_after_other_key() {
        // `text` is not the first key.
        assert_eq!(
            extract_partial_text_from_json("{\"asr\":\"喂\",\"text\":\"你好"),
            Some("你好".to_string())
        );
    }

    #[test]
    fn partial_text_handles_escapes() {
        assert_eq!(
            extract_partial_text_from_json("{\"text\":\"a\\\"b"),
            Some("a\"b".to_string())
        );
        assert_eq!(
            extract_partial_text_from_json("{\"text\":\"line1\\nline2"),
            Some("line1\nline2".to_string())
        );
        assert_eq!(
            extract_partial_text_from_json("{\"text\":\"back\\\\slash"),
            Some("back\\slash".to_string())
        );
    }

    #[test]
    fn partial_text_unicode_escape() {
        assert_eq!(
            extract_partial_text_from_json("{\"text\":\"\\u4f60\\u597d"),
            Some("你好".to_string())
        );
    }

    #[test]
    fn partial_text_no_text_key() {
        assert_eq!(extract_partial_text_from_json("{\"asr\":\"喂\"}"), None);
        assert_eq!(extract_partial_text_from_json(""), None);
    }

    #[test]
    fn partial_text_key_not_substring_of_longer_key() {
        // `"text"` must not match inside `"textarea"` or `"context"`.
        assert_eq!(
            extract_partial_text_from_json("{\"textarea\":\"x\",\"text\":\"actual"),
            Some("actual".to_string())
        );
    }

    #[test]
    fn partial_text_incomplete_key_or_colon() {
        // Key present but colon/quote not yet arrived.
        assert_eq!(extract_partial_text_from_json("{\"text\""), None);
        assert_eq!(extract_partial_text_from_json("{\"text\":"), None);
        assert_eq!(extract_partial_text_from_json("{\"text\": "), None);
        // Opening quote not yet arrived.
        assert_eq!(
            extract_partial_text_from_json("{\"text\":"),
            None
        );
    }

    #[test]
    fn partial_text_empty_value() {
        // Opening quote present, nothing after.
        assert_eq!(
            extract_partial_text_from_json("{\"text\":\""),
            Some(String::new())
        );
    }

    #[test]
    fn partial_text_incremental_growth() {
        // Simulate deltas accumulating: each call returns the value known so
        // far. The stream loop computes the new suffix by stripping the
        // previous return value as a prefix.
        let acc1 = "{\"text\":\"你好";
        let acc2 = "{\"text\":\"你好世界";
        let acc3 = "{\"text\":\"你好世界。";
        let p1 = extract_partial_text_from_json(acc1).unwrap();
        let p2 = extract_partial_text_from_json(acc2).unwrap();
        let p3 = extract_partial_text_from_json(acc3).unwrap();
        assert_eq!(p1, "你好");
        assert_eq!(p2, "你好世界");
        assert_eq!(p3, "你好世界。");
        // New-suffix computation the stream loop performs:
        assert_eq!(p2.strip_prefix(&p1).unwrap(), "世界");
        assert_eq!(p3.strip_prefix(&p2).unwrap(), "。");
    }
}
