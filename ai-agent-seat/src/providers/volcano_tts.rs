//! Volcano TTS provider implementation.
//!
//! Wraps the existing Volcano WebSocket `VolcanoBidirectionalSession` to
//! conform to our dyn-compatible `TtsProvider` trait.
//!
//! Volcano TTS is fire-and-forget: `synthesize_sentence` sends a `task_request`
//! and returns immediately. Audio chunks arrive asynchronously via the
//! `on_audio` callback (which pushes directly into the call-wide SPSC ringbuf).
//! So `synthesize` here returns an empty `Vec<u8>` — the audio has already
//! been routed to the ringbuf by the driver loop. Turn completion is signalled
//! via `on_turn_end` (stream-idle timeout inside the driver).
//!
//! # Concurrency
//!
//! `synthesize` locks the outer `Mutex` ONLY for lazy init (first call creates
//! the session). Once the session exists it is cloned out (cheap — `Arc` inner)
//! and the lock is dropped BEFORE `synthesize_sentence` is awaited. This lets
//! per-sentence dispatch overlap: sentence 2's `synthesize_sentence` can start
//! as soon as sentence 1 releases the session's internal `send_mutex` (held
//! only for the WS send, not the audio playback). Audio order is preserved by
//! the server's FIFO on the call-lifetime session + the single ringbuf sink.

use crate::audio_dsp::OnAudio;
use crate::providers::TtsProvider;
use crate::tts::{OnTurnEnd, VolcanoBidirectionalSession};
use anyhow::Result;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::debug;

/// Volcano TTS provider using the WebSocket bidirectional protocol.
///
/// Holds a lazily-initialized `VolcanoBidirectionalSession` (one per call).
/// The `on_audio` callback pushes PCM directly into the caller's ringbuf —
/// `synthesize` does NOT collect audio bytes (returns empty Vec) because the
/// audio is already being played by the driver loop.
pub struct VolcanoTtsProvider {
    session: Arc<Mutex<Option<VolcanoBidirectionalSession>>>,
    endpoint: String,
    api_key: String,
    resource_id: String,
    speaker: String,
    call_uuid: String,
    on_audio: Mutex<OnAudio>,
    on_turn_end: Mutex<OnTurnEnd>,
    turn_open: std::sync::atomic::AtomicBool,
}

impl VolcanoTtsProvider {
    pub fn new(
        endpoint: String,
        api_key: String,
        resource_id: String,
        speaker: String,
        call_uuid: String,
        on_audio: OnAudio,
        on_turn_end: OnTurnEnd,
    ) -> Self {
        Self {
            session: Arc::new(Mutex::new(None)),
            endpoint,
            api_key,
            resource_id,
            speaker,
            call_uuid,
            on_audio: Mutex::new(on_audio),
            on_turn_end: Mutex::new(on_turn_end),
            turn_open: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

impl TtsProvider for VolcanoTtsProvider {
    fn synthesize(&self, text: &str) -> Pin<Box<dyn Future<Output = Result<Vec<u8>>> + Send + '_>> {
        let text = text.to_string();
        let session = self.session.clone();

        Box::pin(async move {
            // Lock the outer Mutex ONLY for lazy init (first call creates the
            // session). Once the session exists, clone it out (cheap — Arc inner)
            // and drop the guard BEFORE calling synthesize_sentence. This lets
            // per-sentence dispatch run concurrently: sentence 2's
            // synthesize_sentence can start as soon as sentence 1 releases the
            // session's internal send_mutex (after the WS send), instead of
            // waiting for sentence 1's entire synthesize_sentence to return.
            // Audio order is preserved by the server's FIFO on the call-lifetime
            // session + the single ringbuf sink.
            let sess = {
                let mut guard = session.lock().await;
                if guard.is_none() {
                    debug!("Volcano TTS: creating session for {}", text.chars().count());

                    // Take the callbacks out of their Mutexes to move into the session.
                    // They'll be re-installed only once at construction; subsequent
                    // synthesize calls reuse the same session.
                    let on_audio = {
                        let mut cb = self.on_audio.lock().await;
                        // Replace with a no-op so the Mutex stays valid; the real
                        // callback now lives inside the session.
                        std::mem::replace(&mut *cb, Box::new(|_| {}))
                    };
                    let on_turn_end = {
                        let mut cb = self.on_turn_end.lock().await;
                        std::mem::replace(&mut *cb, Box::new(|| {}))
                    };

                    let s = VolcanoBidirectionalSession::new(
                        self.endpoint.clone(),
                        self.api_key.clone(),
                        self.resource_id.clone(),
                        self.speaker.clone(),
                        self.call_uuid.clone(),
                        on_audio,
                        on_turn_end,
                    );
                    s.start()
                        .await
                        .map_err(|e| anyhow::anyhow!("Volcano TTS start failed: {e}"))?;
                    *guard = Some(s);
                }
                guard.as_ref().unwrap().clone()
            }; // guard dropped here — outer Mutex released.

            // Fire-and-forget: sends task_request, audio arrives via on_audio.
            // Always use turn_open=false — each synthesize call is a new turn
            // (the previous turn's ActiveTask was completed by on_turn_end's
            // idle timeout). The Volcano session is call-lifetime, but each
            // task_request creates a fresh ActiveTask.
            sess.synthesize_sentence(
                &text,
                tokio_util::sync::CancellationToken::new(),
                false,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Volcano TTS synthesize failed: {e}"))?;

            // Audio is played by the driver loop via on_audio → ringbuf.
            // Return empty Vec — orchestrator's synthesize_and_play treats this
            // as "audio is being played, turn_flags will be cleared by on_turn_end".
            Ok(Vec::new())
        })
    }

    fn cancel(&self) {
        self.turn_open
            .store(false, std::sync::atomic::Ordering::Relaxed);
        let session = self.session.clone();
        tokio::spawn(async move {
            let guard = session.lock().await;
            if let Some(sess) = &*guard {
                sess.cancel_current_turn().await;
            }
        });
    }
}
