//! LlmActor - LLM stage of the AI pipeline (ASR → LLM → TTS).
//!
//! Receives recognized text (from ASR), calls the LLM API (placeholder), and
//! forwards the generated response text to the TTS actor
//! ([`TtsActor`](crate::tts::TtsActor)) via a [`Recipient<SynthesizeText>`].
//!
//! Using a `Recipient` (rather than `Addr<TtsActor>`) decouples this module
//! from the concrete TTS actor type: any actor that handles
//! [`SynthesizeText`](crate::tts::SynthesizeText) can be wired in, and the LLM
//! actor need not depend on `tts.rs` beyond the message type itself.
//!
//! The LLM call is a placeholder: it logs the recognized text and returns a
//! canned response. When a real backend is wired in, only
//! [`call_llm`] needs to change.

use actix::prelude::*;
use anyhow::Result;

use crate::tts::SynthesizeText;
use crate::voice_core::Config;

/// Message carrying recognized text from ASR to the LLM actor.
#[derive(Message)]
#[rtype(result = "()")]
pub struct RecognizedText {
    /// The call UUID this turn belongs to.
    pub uuid: String,
    /// Transcribed user speech.
    pub text: String,
}

/// Internal message: record an assistant turn into conversation history.
///
/// The assistant response is produced inside a spawned async task (which can't
/// borrow `&mut self` for the history push); sending it back as a message lets
/// the history mutation happen synchronously on the actor's message loop, the
/// same way [`CallActorImpl`](crate::actor::CallActorImpl) records user turns.
#[derive(Message)]
#[rtype(result = "()")]
struct RecordAssistant {
    content: String,
}

/// A single message in the LLM conversation history.
#[derive(Clone)]
struct ConversationMessage {
    role: String,
    content: String,
}

/// LLM actor: turns recognized text into response text and forwards it to TTS.
///
/// The actor owns the per-call conversation history and the LLM configuration
/// (endpoint, model, api key, system prompt). It is spawned onto the module's
/// actix System (see [`crate::actor`]) and addressed via [`Addr<LlmActor>`].
pub struct LlmActor {
    /// The call UUID this actor owns.
    uuid: String,
    /// LLM endpoint configuration (may be absent if no config loaded).
    config: Option<Config>,
    /// Conversation history for LLM context (system + user + assistant turns).
    conversation: Vec<ConversationMessage>,
    /// Where to send synthesized-text requests. The TTS actor registers this
    /// recipient when wiring the pipeline; if absent, responses are dropped
    /// (logged) rather than sent.
    tts: Option<Recipient<SynthesizeText>>,
}

impl LlmActor {
    /// Create a new LLM actor for the given call UUID.
    ///
    /// If `config` is provided and carries a system prompt, it is seeded as the
    /// first conversation message so subsequent LLM calls include it.
    pub fn new(uuid: String, config: Option<Config>) -> Self {
        let mut conversation = Vec::new();

        if let Some(cfg) = config.as_ref()
            && let Some(system_prompt) = cfg.ai.system_prompt.as_ref()
            && !system_prompt.is_empty()
        {
            conversation.push(ConversationMessage {
                role: "system".to_string(),
                content: system_prompt.clone(),
            });
        }

        Self {
            uuid,
            config,
            conversation,
            tts: None,
        }
    }

    /// Set the TTS recipient the LLM actor forwards responses to.
    pub fn with_tts(mut self, tts: Recipient<SynthesizeText>) -> Self {
        self.tts = Some(tts);
        self
    }

    /// Set the TTS recipient after construction (e.g. once the TTS actor exists).
    pub fn set_tts(&mut self, tts: Recipient<SynthesizeText>) {
        self.tts = Some(tts);
    }

    /// Build an owned snapshot of the conversation for an async LLM call.
    ///
    /// The snapshot is taken at the start of a turn so the spawned future (which
    /// cannot borrow `&self`) has a 'static input; the assistant turn is
    /// recorded back into `self.conversation` after the call resolves via
    /// [`RecordAssistant`].
    fn conversation_snapshot(&self) -> Vec<(String, String)> {
        self.conversation
            .iter()
            .map(|m| (m.role.clone(), m.content.clone()))
            .collect()
    }
}

impl Actor for LlmActor {
    type Context = Context<Self>;

    fn started(&mut self, _ctx: &mut Self::Context) {
        tracing::info!("LlmActor started for session {}", self.uuid);
    }

    fn stopped(&mut self, _ctx: &mut Self::Context) {
        tracing::info!("LlmActor stopped for session {}", self.uuid);
    }
}

/// Handle recognized text from ASR.
///
/// Records the user turn synchronously (mirroring
/// [`CallActorImpl::handle_speech_turn`](crate::actor::CallActorImpl)), then
/// spawns the async LLM call on the actor's own `Context` via
/// [`AsyncContext::spawn`] (rather than `actix::spawn`, which would land on an
/// arbitrary arbiter and race the actor's message loop). The spawned future owns
/// clones of the per-turn inputs (uuid, config, conversation snapshot, TTS
/// recipient) so it is 'static and does not borrow `self`; the assistant turn
/// is recorded back by sending [`RecordAssistant`] to `self`.
impl Handler<RecognizedText> for LlmActor {
    type Result = ();

    fn handle(&mut self, msg: RecognizedText, ctx: &mut Self::Context) -> Self::Result {
        let uuid = msg.uuid.clone();
        let user_text = msg.text;

        tracing::info!("Recognized text for session {}: {:?}", uuid, user_text);

        // Record the user turn synchronously so the conversation history is
        // up-to-date before the snapshot below.
        self.conversation.push(ConversationMessage {
            role: "user".to_string(),
            content: user_text.clone(),
        });

        // Owned inputs for the spawned future.
        let config = self.config.clone();
        let history = self.conversation_snapshot();
        let tts = self.tts.clone();
        let assistant_recipient = ctx.address().recipient();

        ctx.spawn(
            async move {
                match call_llm(&uuid, config.as_ref(), &history, &user_text).await {
                    Ok(response) => {
                        tracing::info!("LLM response for session {}: {:?}", uuid, response);

                        // Forward to the TTS actor (if wired). A missing
                        // recipient is a soft error: the response is logged
                        // but not synthesized.
                        if let Some(tts) = tts.as_ref() {
                            if let Err(e) = tts
                                .send(SynthesizeText {
                                    text: response.clone(),
                                })
                                .await
                            {
                                tracing::error!(
                                    "Failed to send SynthesizeText for session {}: {}",
                                    uuid,
                                    e
                                );
                            }
                        } else {
                            tracing::warn!(
                                "No TTS recipient wired for session {}; dropping response",
                                uuid
                            );
                        }

                        // Record the assistant turn back into the actor's
                        // history so it informs the next turn's context.
                        assistant_recipient.do_send(RecordAssistant { content: response });
                    }
                    Err(e) => {
                        tracing::error!("LLM call failed for session {}: {}", uuid, e);
                    }
                }
            }
            .into_actor(self),
        );
    }
}

/// Record an assistant turn into conversation history (sent back by the
/// spawned LLM task after it resolves).
impl Handler<RecordAssistant> for LlmActor {
    type Result = ();

    fn handle(&mut self, msg: RecordAssistant, _ctx: &mut Self::Context) -> Self::Result {
        self.conversation.push(ConversationMessage {
            role: "assistant".to_string(),
            content: msg.content,
        });
    }
}

/// Call the LLM API for one turn.
///
/// Placeholder implementation: logs the recognized text (plus endpoint/model
/// from `config` when available) and returns a canned response. A real
/// implementation would POST `history` to `config.ai.llm_endpoint` on the
/// global tokio runtime (see [`crate::actor`]) and parse the assistant message
/// from the response. Kept as a free function (not a `&self` method) so the
/// spawned future can call it with owned inputs and remain `'static`.
async fn call_llm(
    uuid: &str,
    config: Option<&Config>,
    history: &[(String, String)],
    user_text: &str,
) -> Result<String> {
    let endpoint = config
        .map(|c| c.ai.llm_endpoint.as_str())
        .unwrap_or("(no config)");
    let model = config
        .map(|c| c.ai.llm_model.as_str())
        .unwrap_or("(no config)");

    tracing::info!(
        "LLM call for session {} (endpoint={}, model={}, history_turns={}): {:?}",
        uuid,
        endpoint,
        model,
        history.len(),
        user_text
    );

    // Placeholder: canned response. Replace with a real HTTP call to
    // `config.ai.llm_endpoint` using `reqwest` (or similar) on the global
    // tokio runtime.
    Ok("This is a placeholder LLM response.".to_string())
}
