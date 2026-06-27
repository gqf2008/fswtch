//! ASR (Automatic Speech Recognition) actor for AI agent seat.
//!
//! [`AsrActor`] is an actix actor that receives audio chunks from the media bug
//! (via [`AudioChunk`] messages), runs speech recognition, and dispatches the
//! recognized text onward to the LLM stage.
//!
//! The current implementation is a placeholder: it logs the length of each
//! incoming audio chunk and returns empty recognized text. Wiring up a real ASR
//! backend (e.g. a uniMRCP / Whisper / streaming endpoint) replaces the body of
//! [`Handler<AudioChunk>`] and [`AsrActor::recognize`].

use actix::prelude::*;
use anyhow::Result;

use crate::call_core::{SpeechTurn, registry};
use crate::voice_core::Config;

/// Pipeline sample rate (16 kHz) used for ASR input.
const ASR_SAMPLE_RATE: u32 = 16000;

/// Message carrying a chunk of caller audio to be recognized.
///
/// Samples are 16-bit signed PCM at [`ASR_SAMPLE_RATE`] (mono). Produced by the
/// media bug / VAD pipeline and sent to the [`AsrActor`].
#[derive(Message)]
#[rtype(result = "()")]
pub struct AudioChunk {
    /// 16-bit signed PCM samples at [`ASR_SAMPLE_RATE`].
    pub samples: Vec<i16>,
    /// Sample rate of `samples` in Hz (normally [`ASR_SAMPLE_RATE`]).
    pub sample_rate: u32,
    /// UUID of the call this audio belongs to. Used to route the recognized
    /// text back to the owning [`crate::actor::CallActorImpl`] via the registry.
    pub uuid: String,
}

/// ASR actor.
///
/// Owns the speech-recognition state for a single call. Audio chunks arrive as
/// [`AudioChunk`] messages; once a complete speech turn has been recognized the
/// actor emits a [`SpeechTurn`] to the call's [`crate::actor::CallActorImpl`]
/// (looked up in the [`registry`]), which drives the downstream LLM step.
///
/// This is a placeholder implementation: it logs the incoming audio length and
/// produces empty recognized text.
pub struct AsrActor {
    /// UUID of the call this actor serves.
    uuid: String,
    /// Loaded configuration (may be `None` when the module started without one).
    #[allow(dead_code)]
    config: Option<Config>,
    /// Accumulated unrecognized audio for the current speech turn.
    pending: Vec<i16>,
}

impl AsrActor {
    /// Create a new [`AsrActor`] for the given call UUID.
    pub fn new(uuid: String, config: Option<Config>) -> Self {
        Self {
            uuid,
            config,
            pending: Vec::new(),
        }
    }

    /// Recognize a complete speech turn from accumulated audio.
    ///
    /// Placeholder: logs the sample count and returns an empty string. A real
    /// implementation would forward `audio` to an ASR backend (uniMRCP, a
    /// streaming HTTP endpoint, an on-device model, ...) and return the
    /// transcribed text.
    fn recognize(&self, audio: &[i16]) -> Result<String> {
        tracing::info!(
            "ASR recognize for session {}: {} samples @ {} Hz",
            self.uuid,
            audio.len(),
            ASR_SAMPLE_RATE
        );
        // Placeholder: no real ASR backend wired up yet.
        Ok(String::new())
    }

    /// Flush the accumulated audio as a completed speech turn.
    ///
    /// Runs recognition over `self.pending`, then forwards the resulting text to
    /// the call's [`crate::actor::CallActorImpl`] (looked up in the registry)
    /// as a [`SpeechTurn`] so the LLM stage can consume it.
    fn flush_turn(&mut self) {
        let audio = std::mem::take(&mut self.pending);
        if audio.is_empty() {
            return;
        }

        let text = match self.recognize(&audio) {
            Ok(text) => text,
            Err(e) => {
                tracing::warn!("ASR recognition failed for session {}: {}", self.uuid, e);
                return;
            }
        };

        tracing::info!(
            "ASR turn complete for session {}: {} chars",
            self.uuid,
            text.len()
        );

        // Dispatch recognized text to the call's CallActor, which will feed the
        // LLM. We look up the actor by UUID; if it has already gone away (call
        // hung up) the turn is silently dropped.
        if let Some(addr) = registry().get(&self.uuid) {
            addr.do_send(SpeechTurn { text, audio });
        } else {
            tracing::debug!(
                "No CallActor for session {} while dispatching ASR turn; dropping",
                self.uuid
            );
        }
    }
}

impl Actor for AsrActor {
    type Context = Context<Self>;

    fn started(&mut self, _ctx: &mut Self::Context) {
        tracing::info!("AsrActor started for session {}", self.uuid);
    }

    fn stopped(&mut self, _ctx: &mut Self::Context) {
        // Flush any trailing audio before going down so a final partial turn is
        // not lost on teardown.
        if !self.pending.is_empty() {
            tracing::info!(
                "AsrActor stopping for session {}; flushing {} pending samples",
                self.uuid,
                self.pending.len()
            );
            self.flush_turn();
        }
        tracing::info!("AsrActor stopped for session {}", self.uuid);
    }
}

/// Handle an incoming audio chunk.
///
/// Accumulates samples in `self.pending`. In this placeholder the audio is not
/// actually segmented into turns (that is the VAD layer's job in
/// [`crate::bug::VoiceSeatBug`]); we simply log the chunk length and keep
/// accumulating until the actor is stopped or a real endpoint signals turn
/// completion. `recognize` is still invoked per chunk via the flush path so the
/// plumbing to the LLM stage can be exercised.
impl Handler<AudioChunk> for AsrActor {
    type Result = ();

    fn handle(&mut self, msg: AudioChunk, _ctx: &mut Self::Context) -> Self::Result {
        tracing::debug!(
            "AsrActor received audio chunk for session {}: {} samples @ {} Hz",
            self.uuid,
            msg.samples.len(),
            msg.sample_rate
        );

        // Log the incoming audio length (placeholder behavior).
        if msg.samples.is_empty() {
            return;
        }

        // Accumulate for a future turn (real ASR would stream this into its
        // decoder). A real implementation would also use the sample rate to
        // resample when it does not match ASR_SAMPLE_RATE.
        self.pending.extend_from_slice(&msg.samples);

        // Placeholder: return empty text. We do not emit a SpeechTurn per chunk
        // — turn boundaries come from the VAD layer. The final flush happens on
        // stop. A real implementation would call self.recognize() when the ASR
        // backend signals end-of-utterance and then dispatch via flush_turn().
        let _ = self.recognize(&msg.samples);
    }
}
