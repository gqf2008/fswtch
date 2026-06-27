//! TtsActor - Text-to-Speech actor for AI agent seat.
//!
//! This module implements the [`TtsActor`], an actix [`Actor`] that consumes
//! text produced by the LLM stage of the pipeline, calls out to a TTS API
//! (currently a placeholder), and forwards the resulting 16 kHz mono i16 PCM
//! chunks to the [`VoiceSeatBug`](crate::bug::VoiceSeatBug) through a
//! `tokio::sync::mpsc` channel.
//!
//! The actor→media boundary mirrors the reference `mod_voice_seat` design:
//! the TTS actor owns the producer end of the channel and pushes
//! [`TtsSignal::Chunk`] / [`TtsSignal::ClearBuffer`] messages; the media bug's
//! `on_write_replace` callback drains the receiver end at the FreeSWITCH 20 ms
//! frame cadence. `ClearBuffer` lets the actor cancel playback immediately on
//! barge-in (samples already buffered in the bug are discarded).
//!
//! The implementation here is a **placeholder**: [`synthesize`] only logs the
//! incoming text and returns an empty sample buffer, so the caller hears
//! silence. Wiring a real TTS provider (e.g. ElevenLabs) is a drop-in
//! replacement of [`TtsActor::synthesize`].

use actix::prelude::*;
use anyhow::Result;
use tokio::sync::mpsc;

use crate::audio_dsp::PIPELINE_SAMPLE_RATE;
use crate::voice_core::Config;

/// Maximum number of TTS chunks buffered in the channel before the producer
/// (this actor) is back-pressured. Matches the reference implementation's
/// channel depth so a fast TTS cannot overrun the FS media thread.
const TTS_CHANNEL_CAPACITY: usize = 64;

/// Size (in 16 kHz mono i16 samples) of each chunk pushed to the media bug.
///
/// 320 samples = 20 ms at 16 kHz, which lines up with FreeSWITCH's fixed 20 ms
/// write-frame cadence so the bug can write roughly one chunk per
/// `on_write_replace` call without splitting frames.
const TTS_CHUNK_SAMPLES: usize = 320;

/// Signals crossing the actor → media-bug boundary for TTS playback.
///
/// The actor produces these; the [`VoiceSeatBug`](crate::bug::VoiceSeatBug)
/// consumes them on the FreeSWITCH media thread.
#[derive(Debug)]
pub enum TtsSignal {
    /// A chunk of 16 kHz mono i16 TTS audio to play toward the caller.
    Chunk(Vec<i16>),
    /// Discard all buffered TTS audio immediately (barge-in). Stops the AI's
    /// mouth now — chunks already queued in the bug are dropped.
    ClearBuffer,
}

/// Message: synthesize `text` into speech and stream the audio to the bug.
#[derive(Message)]
#[rtype(result = "()")]
pub struct SynthesizeText {
    /// Text to synthesize (LLM output / assistant turn).
    pub text: String,
}

/// Message: clear any buffered TTS audio (barge-in).
#[derive(Message)]
#[rtype(result = "()")]
pub struct ClearTtsBuffer;

/// TTS actor.
///
/// Owns the producer end of the TTS mpsc channel. Text arrives via
/// [`SynthesizeText`], is run through [`TtsActor::synthesize`] (placeholder
/// TTS API call), and the resulting 16 kHz i16 PCM is chunked and pushed to
/// the bug as [`TtsSignal::Chunk`] messages.
pub struct TtsActor {
    /// UUID of the call this actor serves.
    uuid: String,
    /// Loaded module configuration (TTS endpoint / API key live under `ai`).
    config: Option<Config>,
    /// Producer end of the TTS channel; the consumer is held by the media bug.
    tts_tx: mpsc::Sender<TtsSignal>,
}

impl TtsActor {
    /// Create a new `TtsActor` and the channel the media bug will drain.
    ///
    /// Returns the actor and the receiver half of the TTS channel. The caller
    /// (the app entrypoint that builds the bug) threads the receiver into
    /// [`VoiceSeatBug`](crate::bug::VoiceSeatBug) so its `on_write_replace`
    /// callback can pull chunks at frame cadence.
    pub fn new(uuid: String, config: Option<Config>) -> (Self, mpsc::Receiver<TtsSignal>) {
        let (tts_tx, tts_rx) = mpsc::channel::<TtsSignal>(TTS_CHANNEL_CAPACITY);
        let actor = Self {
            uuid,
            config,
            tts_tx,
        };
        (actor, tts_rx)
    }

    /// Create a `TtsActor` wrapping an existing channel sender.
    ///
    /// Use this when the channel pair is created elsewhere (e.g. the call
    /// launcher) and only the actor needs the producer end.
    pub fn with_sender(
        uuid: String,
        config: Option<Config>,
        tts_tx: mpsc::Sender<TtsSignal>,
    ) -> Self {
        Self {
            uuid,
            config,
            tts_tx,
        }
    }

    /// Synthesize `text` into 16 kHz mono i16 PCM.
    ///
    /// **Placeholder**: logs the text and returns an empty buffer. A real
    /// implementation would issue an HTTP request to the configured TTS
    /// endpoint (`config.ai.tts_endpoint` / `config.ai.tts_api_key`) and
    /// decode the returned audio to i16 at [`PIPELINE_SAMPLE_RATE`].
    async fn synthesize(&self, text: &str) -> Result<Vec<i16>> {
        tracing::info!(
            "TTS placeholder: synthesizing {} bytes of text for session {}",
            text.len(),
            self.uuid
        );

        if let Some(cfg) = &self.config {
            tracing::debug!(
                "TTS endpoint would be: {}/{}",
                cfg.ai.tts_endpoint,
                cfg.ai.tts_api_key.len()
            );
        }

        // Placeholder: no audio produced yet. Returning an empty vec lets the
        // pipeline run end-to-end without a TTS provider; the caller simply
        // hears silence until a real backend is wired in.
        Ok(Vec::new())
    }

    /// Push a synthesized audio buffer to the media bug in
    /// [`TTS_CHUNK_SAMPLES`]-sized chunks.
    ///
    /// The bug drains one 20 ms frame per `on_write_replace`, so chunking at
    /// the frame boundary keeps playback smooth and avoids a single oversized
    /// `Vec` sitting in the channel. Splits the input greedily; the final chunk
    /// may be shorter than a full frame.
    async fn send_audio(&self, audio: &[i16]) {
        if audio.is_empty() {
            return;
        }

        let mut start = 0;
        while start < audio.len() {
            let end = (start + TTS_CHUNK_SAMPLES).min(audio.len());
            let chunk = audio[start..end].to_vec();
            if self.tts_tx.send(TtsSignal::Chunk(chunk)).await.is_err() {
                // Receiver dropped — the bug closed (call hung up). Stop
                // pushing; remaining chunks are irrelevant.
                tracing::warn!(
                    "TTS channel closed for session {}; stopping playback",
                    self.uuid
                );
                return;
            }
            start = end;
        }
    }
}

impl Actor for TtsActor {
    type Context = Context<Self>;

    fn started(&mut self, _ctx: &mut Self::Context) {
        tracing::info!("TtsActor started for session {}", self.uuid);
    }

    fn stopped(&mut self, _ctx: &mut Self::Context) {
        tracing::info!("TtsActor stopped for session {}", self.uuid);
    }
}

/// Handle a request to synthesize text and stream it to the bug.
///
/// The async synthesis + chunked-send is spawned on the actor's own
/// [`Context`] via [`AsyncContext::spawn`] (and `.into_actor(self)`) so the
/// future is pinned to this actor's `LocalSet` with `&mut self` access,
/// rather than landing on an arbitrary arbiter via `actix::spawn`.
impl Handler<SynthesizeText> for TtsActor {
    type Result = ();

    fn handle(&mut self, msg: SynthesizeText, ctx: &mut Self::Context) -> Self::Result {
        let uuid = self.uuid.clone();
        let text_len = msg.text.len();
        tracing::info!(
            "TtsActor: synthesize request for session {} ({} bytes)",
            uuid,
            text_len
        );

        // Borrow the sender for the spawned future. `mpsc::Sender` is `Clone`,
        // so we clone it (cheap — it's an `Arc` internally) to get a 'static
        // handle owned by the future, avoiding a borrow on `self`.
        let tts_tx = self.tts_tx.clone();
        let config = self.config.clone();
        let uuid_for_task = uuid.clone();

        ctx.spawn(
            async move {
                let actor = TtsActor {
                    uuid: uuid_for_task,
                    config,
                    tts_tx,
                };

                match actor.synthesize(&msg.text).await {
                    Ok(audio) => {
                        tracing::info!(
                            "TTS produced {} samples for session {}",
                            audio.len(),
                            actor.uuid
                        );
                        actor.send_audio(&audio).await;
                    }
                    Err(e) => {
                        tracing::error!("TTS synthesis failed for session {}: {}", actor.uuid, e);
                    }
                }
            }
            .into_actor(self),
        );
    }
}

/// Handle a barge-in / cancel request: tell the bug to drop buffered audio.
impl Handler<ClearTtsBuffer> for TtsActor {
    type Result = ();

    fn handle(&mut self, _msg: ClearTtsBuffer, ctx: &mut Self::Context) -> Self::Result {
        tracing::info!("Clearing TTS buffer for session {}", self.uuid);

        let tts_tx = self.tts_tx.clone();
        let uuid = self.uuid.clone();
        ctx.spawn(
            async move {
                if tts_tx.send(TtsSignal::ClearBuffer).await.is_err() {
                    tracing::warn!("TTS channel closed for session {}; clear dropped", uuid);
                }
            }
            .into_actor(self),
        );
    }
}

/// Returns the sample rate TTS audio is produced at (16 kHz).
///
/// The media bug downsamples 16 kHz → codec rate (e.g. 8 kHz) on the write
/// leg using its own resampler. TTS audio must already be at this rate.
pub fn tts_sample_rate() -> u32 {
    PIPELINE_SAMPLE_RATE
}
