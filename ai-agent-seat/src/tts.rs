//! Volcano bidirectional WebSocket TTS client.
//!
//! ONE WebSocket connection + ONE Volcano session per CALL (call-lifetime).
//! [`start`] (call-answer time) connects the WS, sends `start_connection`, and
//! opens the session with `start_session` ONCE. Every turn's [`synthesize`]
//! sends a single `task_request` and returns immediately (fire-and-forget) —
//! it does NOT send `start_session`/`finish_session` per turn and does NOT
//! await a server end signal. `finish_session` + `finish_connection` are sent
//! only at `Drop` (call end).
//!
//! `session_id` and `section_id` are BOTH the call UUID (passed in). The
//! server correlates cross-turn context via `section_id`.
//!
//! # Completion: stream-idle timeout (no server end signal)
//!
//! seed-tts-2.0 sends NO reliable per-task_request end signal in
//! call-lifetime mode (verified against the real server: no `TTSSentenceEnd`,
//! no terminal-packet flag — `TTSSentenceEnd` only appears *after*
//! `finish_session`, which we never send mid-call). So the driver detects
//! turn completion itself: once no `AudioOnlyServer` frame arrives for
//! [`TTS_IDLE_TIMEOUT`] after the first chunk, the turn is considered done and
//! [`on_turn_end`](VolcanoBidirectionalSession::new) fires (the caller uses it
//! to clear `ai_speaking`, unblocking barge-in). The session stays open.
//!
//! # Audio path
//!
//! The driver loop pushes each `AudioOnlyServer` PCM chunk (resampled to the
//! pipeline rate) through the call-wide `on_audio` callback (→ SPSC ringbuf →
//! `io::read_frame`). One callback for the whole call; the `ActiveTask` slot
//! only gates whether the current turn is cancelled (barge-in discards).
//!
//! # Barge-in
//!
//! [`cancel_current_turn`] sets `ActiveTask.cancelled = true`; the driver then
//! discards in-flight audio for that turn (keeps draining the server cleanly).
//! A new turn's `synthesize` overwrites `current` with a fresh task. The
//! connection + session stay alive either way — only `Drop` tears them down.

use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::handshake::client::Request;

use crate::audio_dsp::{OnAudio, PIPELINE_SAMPLE_RATE};
use crate::tts_ws_codec::{EventType, Message, MsgType};

/// Turn-completion callback: the driver invokes this once the current turn's
/// audio stream goes idle (no server audio for [`TTS_IDLE_TIMEOUT`] after the
/// first chunk). The caller installs a closure that clears `ai_speaking`,
/// unblocking barge-in detection.
pub type OnTurnEnd = Box<dyn FnMut() + Send + 'static>;

// Reuse the shared `SendResample` wrapper from `audio_dsp` (also used by the
// VAD bypass in `io.rs`) — one `unsafe impl Send` in the whole crate.
use crate::audio_dsp::SendResample;

/// The default Volcano bidirectional TTS WebSocket endpoint.
const DEFAULT_ENDPOINT: &str = "wss://openspeech.bytedance.com/api/v3/tts/bidirection";

/// The sample rate (Hz) we request from the Volcano TTS server AND the pipeline
/// rate. We request 8 kHz to match the pipeline natively — no resampler needed.
/// Python test verified: 8 kHz + finish_session → TTSSentenceEnd arrives OK.
const TTS_SERVER_SAMPLE_RATE: u32 = 8000;

/// How long with no `AudioOnlyServer` frame (after the first chunk) before the
/// driver declares the current turn's audio complete and fires `on_turn_end`.
/// Must exceed the server's inter-chunk gap (~50-100 ms) and be well under the
/// 60 s hard net. Tuned conservatively — a mid-utterance pause longer than this
/// would fire early, but TTS streams continuously within a task_request.
const TTS_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(400);

/// How long after a `task_request` with NO audio frame received before the
/// driver declares the turn complete anyway (bounds a server that accepted the
/// request but never streamed audio — silent drop / read-path stall). Without
/// this, `ai_speaking` sticks true forever (the idle timer only arms after the
/// first chunk). Generous: the server's first-chunk latency is typically
/// 100-400 ms, so a real reply always arms the idle timer before this fires.
const TTS_FIRST_AUDIO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Command sent to the driver task that owns the socket.
enum DriverCmd {
    Send(Message),
    Shutdown,
}

/// Internal session state shared between the API surface and the driver task.
struct SessionState {
    /// `true` once `start_session` is acknowledged (`SessionStarted` observed).
    /// Reset only on failure / socket death — NOT per turn (the session is
    /// call-lifetime). `ensure_started` reopens it when this is false.
    started: bool,
    /// The currently-active turn. Audio frames are routed here unless its
    /// `cancelled` flag is set. `None` when no turn is active.
    current: Option<ActiveTask>,
}

/// The active turn's cancellation + completion state.
struct ActiveTask {
    cancelled: bool,
    start: std::time::Instant,
    /// Flips to `true` on the first `AudioOnlyServer` frame for this turn. The
    /// `swap` return value ("was this the first?") doubles as the first-chunk
    /// latency log trigger AND arms the idle timer (which must not fire during
    /// the server's first-chunk latency).
    first_audio_received: std::sync::atomic::AtomicBool,
    /// Once `true`, `on_turn_end` has fired for this task; the idle timer must
    /// not re-fire it. Guards against the deadline being repeatedly met after
    /// completion (last_audio_at stops updating once the stream is idle).
    completed: std::sync::atomic::AtomicBool,
    /// Instant of the last received `AudioOnlyServer` frame for this turn.
    /// The idle timer fires when `now - last_audio_at >= TTS_IDLE_TIMEOUT`.
    last_audio_at: parking_lot::Mutex<Option<std::time::Instant>>,
}

impl ActiveTask {
    fn new() -> Self {
        Self {
            cancelled: false,
            start: std::time::Instant::now(),
            first_audio_received: std::sync::atomic::AtomicBool::new(false),
            completed: std::sync::atomic::AtomicBool::new(false),
            last_audio_at: parking_lot::Mutex::new(None),
        }
    }
}

/// A bidirectional Volcano TTS session bound to one call.
///
/// Constructed cheaply (sync) with the call UUID; the WebSocket connect +
/// `start_session` happen in [`start`](Self::start) (call-answer time) or
/// lazily on the first `synthesize` if `start` was not called. Cloning is cheap
/// (Arc inner).
#[derive(Clone)]
pub struct VolcanoBidirectionalSession {
    inner: Arc<SessionInner>,
}

struct SessionInner {
    endpoint: String,
    api_key: String,
    resource_id: String,
    speaker: String,
    call_uuid: String,
    send_mutex: Mutex<()>,
    state: Arc<Mutex<SessionState>>,
    cmd_tx: parking_lot::Mutex<Option<tokio::sync::mpsc::Sender<DriverCmd>>>,
    driver_join: parking_lot::Mutex<Option<tokio::task::JoinHandle<()>>>,
    started_flag: std::sync::atomic::AtomicBool,
    /// Direct audio callback invoked by the driver loop with each PCM chunk.
    /// Wraps `FnMut` in a `parking_lot::Mutex` because the driver runs the
    /// callback from an async context (`route_frame` / `driver_loop`) and needs
    /// `&mut`; the lock is held only for the duration of the push (fast).
    on_audio: parking_lot::Mutex<OnAudio>,
    /// Invoked by the driver loop when the current turn's audio stream goes
    /// idle (turn complete) — the caller clears `ai_speaking`. Same locking
    /// rationale as `on_audio`.
    on_turn_end: parking_lot::Mutex<OnTurnEnd>,
}

impl VolcanoBidirectionalSession {
    /// Build a session for the given credentials + speaker, bound to the call
    /// UUID. The server always outputs at `TTS_SERVER_SAMPLE_RATE` (8 kHz); the
    /// UUID is used for BOTH `session_id` and `section_id` (cross-turn
    /// correlation). The connect happens later in [`start`] or lazily in
    /// [`synthesize`].
    ///
    /// `on_audio` is invoked by the driver loop for each PCM chunk — the caller
    /// (actor) pushes it straight into its ringbuf producer, eliminating the old
    /// `mpsc` indirection. The callback is `FnMut` (held under a short-lived
    /// lock) so it can mutate internal buffer state.
    ///
    /// `on_turn_end` is invoked by the driver loop once a turn's audio stream
    /// goes idle — the caller clears `ai_speaking` so barge-in detection
    /// resumes.
    pub fn new(
        endpoint: String,
        api_key: String,
        resource_id: String,
        speaker: String,
        call_uuid: String,
        on_audio: OnAudio,
        on_turn_end: OnTurnEnd,
    ) -> Self {
        tracing::debug!(
            "Volcano TTS session constructed: call_uuid={} endpoint={} (session_id=section_id=call_uuid)",
            call_uuid,
            endpoint,
        );
        Self {
            inner: Arc::new(SessionInner {
                endpoint,
                api_key,
                resource_id,
                speaker,
                call_uuid,
                send_mutex: Mutex::new(()),
                state: Arc::new(Mutex::new(SessionState {
                    started: false,
                    current: None,
                })),
                cmd_tx: parking_lot::Mutex::new(None),
                driver_join: parking_lot::Mutex::new(None),
                started_flag: std::sync::atomic::AtomicBool::new(false),
                on_audio: parking_lot::Mutex::new(on_audio),
                on_turn_end: parking_lot::Mutex::new(on_turn_end),
            }),
        }
    }

    /// Eagerly establish the WebSocket connection AND open the call-lifetime
    /// session (`start_connection` + `start_session`, once). Called at
    /// call-answer time for pre-warming. Idempotent + race-safe. If it fails,
    /// `synthesize` lazy-retries via `ensure_started`.
    pub async fn start(&self) -> Result<()> {
        if self
            .inner
            .started_flag
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Ok(());
        }
        let _send_guard = self.inner.send_mutex.lock().await;
        if self
            .inner
            .started_flag
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Ok(());
        }
        self.release_stale_driver_if_any();
        // Driver alive but `started_flag` still false → a prior `start()`
        // connected but hasn't finished opening the session. Await that
        // in-flight start rather than opening a second one.
        if self.inner.cmd_tx.lock().is_some() {
            drop(_send_guard);
            return self.wait_for_started().await;
        }

        // Connect + spawn driver + start_connection.
        if let Err(e) = self.connect_and_spawn().await {
            tracing::warn!("Volcano TTS start() connect failed: {e}");
            return Err(e);
        }
        // Open the call-lifetime session ONCE (not per turn).
        if let Err(e) = self.start_session().await {
            tracing::warn!("Volcano TTS start() first start_session failed: {e}");
            return Err(e);
        }
        self.inner
            .started_flag
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    /// Ensure the WebSocket connection is up + a session is open (call-lifetime
    /// — the session is opened once and reused; reopened only after a failure /
    /// socket death reset `started`). Reconnects if the driver died. Used by
    /// `synthesize` as the lazy fallback when `start` was never called or
    /// failed, AND to recover after a mid-call socket break. Idempotent under
    /// `send_mutex`.
    async fn ensure_started(&self) -> Result<()> {
        if self.is_started().await {
            return Ok(());
        }
        let _send_guard = self.inner.send_mutex.lock().await;
        if self.is_started().await {
            return Ok(());
        }
        self.release_stale_driver_if_any();
        // Driver alive (connection up) but no session open (`started=false`
        // after the previous socket death / failure) → open one. Under
        // `send_mutex` no other caller can be mid-`start_session`.
        if self.inner.cmd_tx.lock().is_some() {
            return self.start_session().await;
        }
        // No driver at all — connect + open the first session.
        self.connect_and_spawn().await?;
        self.start_session().await?;
        self.inner
            .started_flag
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    /// Check if a Volcano session is currently open (SessionStarted observed,
    /// not yet torn down).
    async fn is_started(&self) -> bool {
        self.inner.state.lock().await.started
    }

    async fn wait_for_started(&self) -> Result<()> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            if self.is_started().await {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err(anyhow::anyhow!(
                    "Volcano TTS start_session timed out waiting for SessionStarted"
                ));
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    fn release_stale_driver_if_any(&self) {
        let stale = self
            .inner
            .driver_join
            .lock()
            .as_ref()
            .is_some_and(|h| h.is_finished());
        if stale {
            tracing::info!(
                "Volcano TTS session broke; releasing socket, will reconnect on next speak"
            );
            *self.inner.cmd_tx.lock() = None;
            *self.inner.driver_join.lock() = None;
            self.inner
                .started_flag
                .store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }

    async fn connect_and_spawn(&self) -> Result<()> {
        // rustls 0.23 needs an explicit CryptoProvider installed process-wide.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let connect_id = uuid::Uuid::new_v4().to_string();
        let endpoint = &self.inner.endpoint;
        let uri = endpoint
            .parse::<tokio_tungstenite::tungstenite::http::Uri>()
            .context("Volcano TTS endpoint parse failed")?;
        let host = uri.host().unwrap_or("openspeech.bytedance.com").to_string();

        let req = Request::builder()
            .method("GET")
            .uri(endpoint.as_str())
            .header("Host", &host)
            .header("Upgrade", "websocket")
            .header("Connection", "Upgrade")
            .header(
                "Sec-WebSocket-Key",
                tokio_tungstenite::tungstenite::handshake::client::generate_key(),
            )
            .header("Sec-WebSocket-Version", "13")
            .header("X-Api-Key", &self.inner.api_key)
            .header("X-Api-Resource-Id", &self.inner.resource_id)
            .header("X-Api-Connect-Id", &connect_id)
            .body(())
            .context("Volcano TTS WS request build failed")?;

        tracing::info!(
            "Volcano TTS connecting to WS (host={}, connect_id={})",
            host,
            connect_id
        );

        let (ws_stream, _resp) = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            tokio_tungstenite::connect_async(req),
        )
        .await
        .context("Volcano TTS WS connect timed out (10s)")??;

        tracing::info!(
            "Volcano TTS WS connected (resource={}, connect_id={})",
            self.inner.resource_id,
            connect_id
        );

        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<DriverCmd>(64);
        *self.inner.cmd_tx.lock() = Some(cmd_tx.clone());

        let state = Arc::clone(&self.inner.state);
        // Pass TTS_SERVER_SAMPLE_RATE so driver_loop knows the server's output
        // rate. When it matches PIPELINE_SAMPLE_RATE (both 8 kHz) no resampler
        // is created; if they ever differ, driver_loop resamples between them.
        // Pass a WEAK ref — NOT a strong Arc clone. A strong clone creates a
        // reference cycle (driver_loop holds Arc<SessionInner>, SessionInner::Drop
        // sends Shutdown to stop driver_loop) that prevents Drop from ever firing,
        // leaving the WS open until the server's idle timeout (~84s). Weak breaks
        // the cycle: when the actor drops its Arc, strong count → 0, Drop fires,
        // Shutdown is sent, driver_loop exits.
        let inner = Arc::downgrade(&self.inner);
        let driver = tokio::spawn(driver_loop(
            ws_stream,
            cmd_rx,
            state,
            TTS_SERVER_SAMPLE_RATE,
            inner,
        ));
        *self.inner.driver_join.lock() = Some(driver);

        // If start_connection fails, the driver is running but `started_flag`
        // was never set, so `is_started()` returns false — yet `cmd_tx` is
        // `Some`, which `ensure_started` would mistake for "driver alive" and
        // skip reconnect, hanging every subsequent synthesize on the 20 s
        // `wait_for_started` timeout. Clean up on failure so the next attempt
        // reconnects fresh.
        if let Err(e) = self.send_raw(Message::start_connection()).await {
            tracing::warn!("Volcano TTS start_connection send failed; tearing down driver: {e}");
            // Send Shutdown so the driver exits and closes the WS.
            let _ = cmd_tx.send(DriverCmd::Shutdown).await;
            // Drop our references; release_stale_driver_if_any will pick up the
            // finished JoinHandle on the next connect attempt.
            *self.inner.cmd_tx.lock() = None;
            // Detach the JoinHandle (driver is shutting down via the Shutdown cmd).
            *self.inner.driver_join.lock() = None;
            return Err(e);
        }
        Ok(())
    }

    /// Send `start_session` with the call-stable req_params (speaker, pcm, 8k,
    /// section_id) and wait for `SessionStarted` (flipped by the driver). Opens
    /// the call-lifetime session — called once at `start` / on reconnect.
    async fn start_session(&self) -> Result<()> {
        let payload = serde_json::json!({
            "req_params": {
                "speaker": self.inner.speaker,
                "audio_params": {
                    "format": "pcm",
                    "sample_rate": TTS_SERVER_SAMPLE_RATE,
                },
                "section_id": self.inner.call_uuid,
            }
        });
        let payload_bytes = serde_json::to_vec(&payload)?;
        // Reset the started flag BEFORE sending start_session so `wait_for_started`
        // waits for THIS start's SessionStarted — not a stale `true` left over
        // from a prior open session.
        {
            let mut st = self.inner.state.lock().await;
            st.started = false;
        }
        let msg = Message::start_session(payload_bytes, &self.inner.call_uuid);
        self.send_raw(msg).await?;
        self.wait_for_started().await
    }

    async fn send_raw(&self, msg: Message) -> Result<()> {
        let tx = self
            .inner
            .cmd_tx
            .lock()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Volcano TTS session not connected"))?;
        tx.send(DriverCmd::Send(msg))
            .await
            .map_err(|_| anyhow::anyhow!("Volcano TTS driver task closed"))?;
        Ok(())
    }

    /// Send a `task_request` for `text` and return immediately (fire-and-forget).
    /// The driver loop pushes the server's audio through `on_audio` as it
    /// arrives; `on_turn_end` fires once the stream goes idle.
    ///
    /// Does NOT send `start_session`/`finish_session` (call-lifetime session,
    /// opened once at [`start`]) and does NOT await a server end signal
    /// (seed-tts-2.0 sends none in this mode). Serialized by `send_mutex` so
    /// only one `task_request` is on the wire at a time.
    ///
    /// `turn_open`: true when this sentence continues an already-open turn (the
    /// streaming-LLM path sends one task_request per sentence). In that case the
    /// turn's `ActiveTask` is REUSED — we don't cancel it, don't install a fresh
    /// one, and don't re-arm flags. The idle timer keeps tracking the same task;
    /// `last_audio_at` refreshes on every frame (in `route_frame`), so inter-
    /// sentence gaps don't trip the idle timer — only the turn's final stream
    /// silence does. Audio order is the server's FIFO on the single session +
    /// the single ringbuf sink, so no per-sentence sequencer is needed.
    ///
    /// `turn_open == false` (turn's first sentence, or whole-reply mode): opens
    /// the turn — installs a fresh `ActiveTask` (cancelling any prior turn still
    /// streaming late frames). The caller sets `tts_audio_active` + `turn_pending`
    /// around this call.
    ///
    /// Returns `Ok(true)` (always, unless the send itself fails) — completion is
    /// observed asynchronously via `on_turn_end` / [`cancel_current_turn`].
    pub async fn synthesize_sentence(
        &self,
        text: &str,
        cancel: tokio_util::sync::CancellationToken,
        turn_open: bool,
    ) -> Result<bool> {
        let t_syn = std::time::Instant::now();
        tracing::info!(
            "Volcano TTS synthesize: {} chars (turn_open={})",
            text.chars().count(),
            turn_open
        );

        if !turn_open {
            // Ensure the WebSocket connection is up + the call-lifetime session
            // open. (On continuation sentences the session is usually already open.)
            self.ensure_started().await?;
            tracing::info!(
                "LATENCY TTS {}: ensure_started = {}ms",
                self.inner.call_uuid,
                t_syn.elapsed().as_millis()
            );
        } else {
            // Continuation sentence: the session was open at turn start, but it
            // can die mid-turn (server idle timeout, network blip, mid-turn
            // SessionFinished). If it did, reopen — otherwise send_raw fails on
            // a dead cmd_tx and the sentence's audio is silently lost.
            if !self.is_started().await {
                tracing::info!(
                    "Volcano TTS: session died mid-turn, reopening for continuation sentence"
                );
                self.ensure_started().await?;
            }
        }

        // Serialize: one task_request on the wire at a time. Held only for the
        // send (not its audio playback) — audio arrives async after we return.
        let _send_guard = self.inner.send_mutex.lock().await;

        if cancel.is_cancelled() {
            return Ok(false);
        }

        if !turn_open {
            // First sentence of a turn: install this turn as the active
            // forwarder, cancelling any PRIOR turn still in `current`. A new
            // turn only arrives after the previous turn's mailbox work finished
            // and `clear_tts` flushed the ringbuf — but the server may still be
            // streaming the previous turn's audio (call-lifetime session, no
            // finish_session). Marking the old task `cancelled` makes
            // `route_frame` discard those late frames instead of pushing them on
            // top of this turn's audio.
            let mut st = self.inner.state.lock().await;
            if let Some(prev) = st.current.as_mut() {
                prev.cancelled = true;
            }
            st.current = Some(ActiveTask::new());
            // turn_open continuation sentences reuse `current` untouched.
        }

        // task_request — send the text to synthesize. session_id == call_uuid.
        let payload = serde_json::json!({
            "req_params": {
                "text": text,
                "speaker": self.inner.speaker,
                "audio_params": {
                    "format": "pcm",
                    "sample_rate": TTS_SERVER_SAMPLE_RATE,
                },
            }
        });
        let payload_bytes = serde_json::to_vec(&payload)?;
        tracing::debug!(
            "Volcano TTS task_request: {}",
            String::from_utf8_lossy(&payload_bytes)
        );
        if let Err(e) = self
            .send_raw(Message::task_request(payload_bytes, &self.inner.call_uuid))
            .await
        {
            self.inner.state.lock().await.current = None;
            return Err(e);
        }

        // Fire-and-forget: task_request is on the wire, the driver will push
        // incoming audio through on_audio. We do NOT wait for a per-sentence end
        // signal — seed-tts-2.0 sends none in call-lifetime mode. The session
        // is call-lifetime; finish_session fires only at Drop. Per-turn
        // completion is signaled to the caller via `on_turn_end` (stream idle).
        Ok(true)
    }

    /// Mark the current turn cancelled (barge-in). The driver then discards
    /// in-flight audio for this turn instead of pushing it through `on_audio`.
    /// The connection + session stay alive; the next `synthesize` overwrites
    /// `current` with a fresh task.
    pub async fn cancel_current_turn(&self) {
        let mut st = self.inner.state.lock().await;
        if let Some(t) = st.current.as_mut() {
            t.cancelled = true;
        }
    }
}

impl Drop for SessionInner {
    fn drop(&mut self) {
        let (cmd_tx, driver_join, call_uuid) = (
            self.cmd_tx.lock().take(),
            self.driver_join.lock().take(),
            self.call_uuid.clone(),
        );
        let attempt = async move {
            if let Some(tx) = &cmd_tx {
                // Best-effort finish_session (closes the call-lifetime session)
                // then finish_connection, then Shutdown the driver.
                let _ = tx
                    .send(DriverCmd::Send(Message::finish_session(&call_uuid)))
                    .await;
                let _ = tx.send(DriverCmd::Send(Message::finish_connection())).await;
                let _ = tx.send(DriverCmd::Shutdown).await;
            }
            if let Some(h) = driver_join {
                let _ = h.await;
            }
        };
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(attempt);
            }
            Err(_) => {
                std::thread::spawn(move || {
                    let rt = match tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    {
                        Ok(rt) => rt,
                        Err(_) => return,
                    };
                    rt.block_on(attempt);
                });
            }
        }
    }
}

// ── Driver task ────────────────────────────────────────────────────────

async fn driver_loop(
    ws_stream: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    mut cmd_rx: tokio::sync::mpsc::Receiver<DriverCmd>,
    state: Arc<Mutex<SessionState>>,
    server_sample_rate: u32,
    inner: std::sync::Weak<SessionInner>,
) {
    use futures::{SinkExt, StreamExt};
    let (mut sink, mut stream) = ws_stream.split();

    // Resample the server's output rate down to the pipeline rate, using
    // FreeSWITCH's native `switch_resample`. One resampler lives for the whole
    // connection; it carries internal state across chunks, so per-turn reset is
    // not needed (the server emits continuous audio within a session).
    let mut resampler: Option<SendResample> = if server_sample_rate != PIPELINE_SAMPLE_RATE {
        match fswtch::Resample::new(server_sample_rate, PIPELINE_SAMPLE_RATE, 1, 1) {
            Ok(r) => {
                tracing::info!(
                    "Volcano TTS resampler: {} Hz -> {} Hz (FreeSWITCH switch_resample)",
                    server_sample_rate,
                    PIPELINE_SAMPLE_RATE,
                );
                Some(SendResample(r))
            }
            Err(e) => {
                tracing::error!(
                    "resample init failed ({} -> {}): {:?}; audio will play at wrong rate",
                    server_sample_rate,
                    PIPELINE_SAMPLE_RATE,
                    e
                );
                None
            }
        }
    } else {
        None
    };

    loop {
        // Turn-completion deadline. Two cases, whichever comes first:
        //  - audio arrived: fire TTS_IDLE_TIMEOUT after the last chunk (stream
        //    went idle).
        //  - audio NOT yet arrived: fire TTS_FIRST_AUDIO_TIMEOUT after the
        //    task_request was sent — bounds a server that accepted the request
        //    but never streamed audio (silent drop / read-path stall). Without
        //    this, `ai_speaking` sticks true forever (the idle timer only arms
        //    after the first chunk).
        // None only when no active task or the task already completed.
        let idle_deadline: Option<tokio::time::Instant> = {
            let st = state.lock().await;
            st.current.as_ref().and_then(|t| {
                if t.completed.load(std::sync::atomic::Ordering::Relaxed) {
                    None
                } else if t
                    .first_audio_received
                    .load(std::sync::atomic::Ordering::Relaxed)
                {
                    t.last_audio_at
                        .lock()
                        .map(|at| tokio::time::Instant::from_std(at + TTS_IDLE_TIMEOUT))
                } else {
                    Some(tokio::time::Instant::from_std(
                        t.start + TTS_FIRST_AUDIO_TIMEOUT,
                    ))
                }
            })
        };
        let idle = match idle_deadline {
            Some(d) => futures::future::Either::Left(tokio::time::sleep_until(d)),
            None => futures::future::Either::Right(std::future::pending::<()>()),
        };

        tokio::select! {
            biased;
            _ = idle => {
                // Re-check under lock: audio may have arrived between the
                // deadline firing and this wake. Only fire if still stalled/idle.
                let still_due = {
                    let st = state.lock().await;
                    if let Some(t) = st.current.as_ref() {
                        !t.completed.load(std::sync::atomic::Ordering::Relaxed)
                            && if t.first_audio_received.load(std::sync::atomic::Ordering::Relaxed) {
                                t.last_audio_at.lock().map_or(false, |at| {
                                    at.elapsed() >= TTS_IDLE_TIMEOUT
                                })
                            } else {
                                t.start.elapsed() >= TTS_FIRST_AUDIO_TIMEOUT
                            }
                    } else {
                        false
                    }
                };
                if still_due {
                    tracing::info!("Volcano TTS turn complete (idle/no-audio timeout)");
                    fire_turn_complete(&state, &inner).await;
                }
            }
            cmd = cmd_rx.recv() => match cmd {
                Some(DriverCmd::Send(msg)) => {
                    match msg.marshal() {
                        Ok(bytes) => {
                            if let Err(e) = sink.send(WsMessage::Binary(bytes.into())).await {
                                tracing::warn!("Volcano TTS WS write failed: {e}");
                                break;
                            }
                        }
                        Err(e) => {
                            tracing::error!("Volcano TTS marshal failed: {e}");
                        }
                    }
                }
                Some(DriverCmd::Shutdown) | None => {
                    let _ = sink.close().await;
                    break;
                }
            },
            frame = stream.next() => match frame {
                Some(Ok(WsMessage::Binary(bytes))) => {
                    match route_frame(&bytes, &state, &mut resampler, &inner).await {
                        Ok(Some(pcm)) => {
                            // Push through `on_audio`. Weak::upgrade fails only
                            // when SessionInner has been dropped (call ended).
                            // In that case the Drop impl already sent (or is
                            // about to send) Shutdown — but if it was lost
                            // (runtime shutdown aborted the Drop's spawn), the
                            // driver would otherwise loop forever draining a
                            // dead connection. Treat upgrade failure as a
                            // terminal signal and exit so the WS/task don't leak.
                            if pcm.is_empty() {
                                // no-op
                            } else if let Some(inner) = inner.upgrade() {
                                let mut cb = inner.on_audio.lock();
                                cb(&pcm);
                            } else {
                                tracing::info!(
                                    "Volcano TTS driver: SessionInner gone, exiting loop"
                                );
                                break;
                            }
                        }
                        Ok(None) => {}
                        Err(e) => {
                            tracing::warn!("Volcano TTS frame route error: {e}");
                        }
                    }
                }
                Some(Ok(WsMessage::Ping(p))) => {
                    let _ = sink.send(WsMessage::Pong(p)).await;
                }
                Some(Ok(WsMessage::Close(_))) => {
                    tracing::info!("Volcano TTS WS closed by server");
                    break;
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    tracing::warn!("Volcano TTS WS read error: {e}");
                    break;
                }
                None => {
                    tracing::info!("Volcano TTS WS stream ended");
                    break;
                }
            }
        }
    }
    // Socket broke / shut down. Mark the session down so the next speak call
    // reconnects (ensure_started sees started=false + a finished driver), and
    // fire turn completion so the caller's `on_turn_end` (ai_speaking clear)
    // runs even if the stream died mid-turn.
    {
        let mut st = state.lock().await;
        st.started = false;
    }
    fire_turn_complete(&state, &inner).await;
    tracing::info!("Volcano TTS driver loop exiting");
}

/// Fire the current turn's completion (idempotent): if `current` exists and
/// hasn't already fired, swap its `completed` flag and invoke `on_turn_end`.
/// Called on stream-idle (driver_loop), on server end/failure events
/// (route_frame), and on socket death (driver exit). `Weak::upgrade` failure
/// means the call already ended — nothing to fire.
async fn fire_turn_complete(state: &Mutex<SessionState>, inner: &std::sync::Weak<SessionInner>) {
    let fire = {
        let st = state.lock().await;
        match st.current.as_ref() {
            Some(t) => !t.completed.swap(true, std::sync::atomic::Ordering::Relaxed),
            None => false,
        }
    };
    if fire && let Some(inner) = inner.upgrade() {
        let mut cb = inner.on_turn_end.lock();
        cb();
    }
}

/// Unmarshal one server frame and route it. Returns `Some(pcm)` when the frame
/// is an `AudioOnlyServer` chunk carrying resampled pipeline-rate PCM that the
/// caller should push through `on_audio`; `None` otherwise (control events,
/// empty chunks, or cancelled turns). Also drives turn-completion: updates
/// `last_audio_at` on each audio chunk and fires `on_turn_end` on end/failure
/// events.
async fn route_frame(
    bytes: &[u8],
    state: &Mutex<SessionState>,
    resampler: &mut Option<SendResample>,
    inner: &std::sync::Weak<SessionInner>,
) -> Result<Option<Vec<i16>>> {
    let msg = Message::unmarshal(bytes).context("Volcano TTS unmarshal failed")?;
    tracing::debug!(
        "Volcano TTS recv: msg_type={:?} event={:?} flag={:?} seq={} payload_len={}",
        msg.msg_type,
        msg.event,
        msg.flag,
        msg.sequence,
        msg.payload.len()
    );
    match msg.msg_type {
        MsgType::FullServerResponse => match msg.event {
            EventType::SessionStarted => {
                tracing::info!("Volcano TTS SessionStarted (session_id={})", msg.session_id);
                state.lock().await.started = true;
            }
            EventType::SessionFinished => {
                // The server ended the session. This is normally only our
                // Drop's `finish_session`, but the server CAN end it mid-call
                // (idle timeout, admin reset, protocol error). In that case the
                // next `synthesize` MUST re-run `start_session` — so reset
                // `started=false` here. `ensure_started` then reopens on the
                // next speak (reusing the live connection). Fire turn
                // completion too so an in-flight turn's `on_turn_end` runs.
                tracing::info!("Volcano TTS SessionFinished — reopening on next speak");
                let mut st = state.lock().await;
                st.started = false;
                drop(st);
                fire_turn_complete(state, inner).await;
            }
            EventType::SessionFailed | EventType::ConnectionFailed => {
                tracing::error!(
                    "Volcano TTS session/connection failed: code={}, payload={}",
                    msg.error_code,
                    String::from_utf8_lossy(&msg.payload)
                );
                // Failure mid-turn: mark the session down (ensure_started will
                // reopen on the next speak) + fire turn completion.
                let mut st = state.lock().await;
                st.started = false;
                drop(st);
                fire_turn_complete(state, inner).await;
            }
            EventType::TTSSentenceStart => {
                tracing::info!("Volcano TTS TTSSentenceStart");
            }
            EventType::TTSSentenceEnd => {
                // Best-effort early completion: if the server does send
                // TTSSentenceEnd (it normally doesn't in call-lifetime mode),
                // fire turn completion immediately rather than waiting for the
                // idle timeout.
                tracing::info!("Volcano TTS TTSSentenceEnd — firing turn complete");
                fire_turn_complete(state, inner).await;
            }
            _ => {
                // Log every unmatched server event at INFO so we can see what
                // the server actually returns — essential for diagnosing TTS
                // completion.
                tracing::info!(
                    "Volcano TTS unhandled event: {:?} code={} payload={}",
                    msg.event,
                    msg.error_code,
                    String::from_utf8_lossy(&msg.payload)
                );
            }
        },
        MsgType::AudioOnlyServer => {
            // One lock: arm/refresh the idle timer on ANY frame (incl. empty
            // and cancelled — the cancelled turn must still see its
            // `last_audio_at` advance so its idle timer fires and retires it)
            // AND read this turn's `cancelled` flag for the push decision. The
            // `first_audio_received` swap return doubles as the first-chunk
            // latency trigger.
            let cancelled = {
                let st = state.lock().await;
                match st.current.as_ref() {
                    Some(task) => {
                        if !task
                            .first_audio_received
                            .swap(true, std::sync::atomic::Ordering::Relaxed)
                        {
                            let ms = task.start.elapsed().as_millis() as f64;
                            tracing::info!("Volcano TTS 首Chunk: {:.1}ms", ms);
                        }
                        *task.last_audio_at.lock() = Some(std::time::Instant::now());
                        task.cancelled
                    }
                    None => true, // no active turn → discard
                }
            };

            // Discard cancelled / empty frames BEFORE the per-frame alloc — the
            // server keeps streaming after barge-in (call-lifetime session, no
            // finish_session), so ~100-250 discarded frames would each allocate
            // a Vec otherwise.
            if cancelled || msg.payload.len() < 2 {
                return Ok(None);
            }

            let samples = parse_pcm_le(&msg.payload);

            // Resample the server-rate PCM down to the pipeline rate using the
            // connection-scoped FreeSWITCH resampler. `.to_vec()` copies out of
            // the resampler's internal buffer before the borrow ends.
            let out: Vec<i16> = if let Some(r) = resampler.as_ref() {
                let mut buf = samples;
                r.0.process(&mut buf).to_vec()
            } else {
                samples
            };
            if out.is_empty() {
                return Ok(None);
            }
            // Hand the chunk to the driver loop (which invokes `on_audio`).
            return Ok(Some(out));
        }
        MsgType::Error => {
            tracing::error!(
                "Volcano TTS server Error: code={}, payload={}",
                msg.error_code,
                String::from_utf8_lossy(&msg.payload)
            );
            // An error mid-session: fire turn completion so the caller's
            // on_turn_end (ai_speaking clear) runs.
            fire_turn_complete(state, inner).await;
        }
        _ => {
            tracing::trace!("Volcano TTS unexpected msg_type: {:?}", msg.msg_type);
        }
    }
    Ok(None)
}

/// Parse raw L16 (little-endian i16) PCM bytes into samples.
fn parse_pcm_le(bytes: &[u8]) -> Vec<i16> {
    let usable = bytes.len() - (bytes.len() % 2);
    bytes[..usable]
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tts_ws_codec::{EventType, Message, MsgType, MsgTypeFlagBits};

    /// Build a `VolcanoBidirectionalSession` WITHOUT connecting (no WS). Lets
    /// tests exercise `cancel_current_turn` + state inspection through the
    /// public constructor + a no-op `on_audio`/`on_turn_end`.
    fn session_no_connect() -> VolcanoBidirectionalSession {
        VolcanoBidirectionalSession::new(
            "key".into(),
            "res".into(),
            "speaker".into(),
            "call-uuid".into(),
            Box::new(|_| {}),
            Box::new(|| {}),
        )
    }

    /// A `VolcanoBidirectionalSession` + its shared state with no-op callbacks.
    /// Use for tests that inspect state but don't assert on audio capture.
    fn session_and_state() -> (VolcanoBidirectionalSession, Arc<Mutex<SessionState>>) {
        let session = session_no_connect();
        let state = Arc::clone(&session.inner.state);
        (session, state)
    }

    // ── parse_pcm_le ─────────────────────────────────────────────────────

    #[test]
    fn parse_pcm_le_empty() {
        assert!(parse_pcm_le(&[]).is_empty());
    }

    #[test]
    fn parse_pcm_le_odd_byte_dropped() {
        // 3 bytes → 1 sample + 1 trailing byte dropped.
        let out = parse_pcm_le(&[0x64, 0x00, 0xFF]);
        assert_eq!(out, vec![100]);
    }

    #[test]
    fn parse_pcm_le_aligned() {
        let bytes: Vec<u8> = [100i16, 200, 300]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        assert_eq!(parse_pcm_le(&bytes), vec![100, 200, 300]);
    }

    // ── cancel_current_turn ──────────────────────────────────────────────

    #[tokio::test]
    async fn cancel_current_turn_sets_cancelled_flag() {
        let session = session_no_connect();
        // Install an active task manually.
        {
            let mut st = session.inner.state.lock().await;
            st.current = Some(ActiveTask::new());
        }
        session.cancel_current_turn().await;
        let st = session.inner.state.lock().await;
        let task = st.current.as_ref().expect("task present");
        assert!(task.cancelled, "cancelled flag must be set");
    }

    #[tokio::test]
    async fn cancel_current_turn_noop_without_task() {
        // No active task — must not panic.
        let session = session_no_connect();
        session.cancel_current_turn().await;
        let st = session.inner.state.lock().await;
        assert!(st.current.is_none());
    }

    // ── fire_turn_complete ───────────────────────────────────────────────

    #[tokio::test]
    async fn fire_turn_complete_invokes_on_turn_end_once() {
        let fired = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let session = VolcanoBidirectionalSession::new(
            "key".into(),
            "res".into(),
            "speaker".into(),
            "call-uuid".into(),
            Box::new(|_| {}),
            {
                let fired = Arc::clone(&fired);
                Box::new(move || {
                    fired.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                })
            },
        );
        let state = Arc::clone(&session.inner.state);
        {
            let mut st = state.lock().await;
            st.current = Some(ActiveTask::new());
        }
        let weak: std::sync::Weak<SessionInner> = Arc::downgrade(&session.inner);
        fire_turn_complete(&state, &weak).await;
        fire_turn_complete(&state, &weak).await; // idempotent — must not re-fire
        assert_eq!(
            fired.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "on_turn_end fires exactly once per task"
        );
    }

    #[tokio::test]
    async fn fire_turn_complete_noop_without_task() {
        let session = session_no_connect();
        let state = Arc::clone(&session.inner.state);
        let weak: std::sync::Weak<SessionInner> = Arc::downgrade(&session.inner);
        // No task installed — must not panic / fire callback.
        fire_turn_complete(&state, &weak).await;
    }

    // ── route_frame ──────────────────────────────────────────────────────

    /// Build a marshalled server frame.
    fn server_frame(msg: Message) -> Vec<u8> {
        msg.marshal().expect("marshal test frame")
    }

    #[tokio::test]
    async fn route_frame_audio_pushes_to_on_audio() {
        let (session, state) = session_and_state();
        let weak = Arc::downgrade(&session.inner);
        {
            let mut st = state.lock().await;
            st.current = Some(ActiveTask::new());
        }
        let pcm: Vec<i16> = vec![100, 200, 300, 400];
        let pcm_bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        let frame = server_frame(Message {
            msg_type: MsgType::AudioOnlyServer,
            flag: MsgTypeFlagBits::PositiveSeq,
            sequence: 1,
            payload: pcm_bytes,
            ..Message::default()
        });
        let mut resampler = None;
        let out = route_frame(&frame, &state, &mut resampler, &weak)
            .await
            .unwrap();
        assert_eq!(out, Some(pcm), "audio PCM routed to caller");
        // The first-audio flag + last_audio_at must be armed.
        let st = state.lock().await;
        let task = st.current.as_ref().unwrap();
        assert!(
            task.first_audio_received
                .load(std::sync::atomic::Ordering::Relaxed),
            "first_audio_received armed"
        );
        assert!(task.last_audio_at.lock().is_some(), "last_audio_at stamped");
    }

    #[tokio::test]
    async fn route_frame_audio_cancelled_is_discarded() {
        let (session, state) = session_and_state();
        let weak = Arc::downgrade(&session.inner);
        {
            let mut st = state.lock().await;
            let mut task = ActiveTask::new();
            task.cancelled = true;
            st.current = Some(task);
        }
        let pcm_bytes: Vec<u8> = [100i16, 200].iter().flat_map(|s| s.to_le_bytes()).collect();
        let frame = server_frame(Message {
            msg_type: MsgType::AudioOnlyServer,
            flag: MsgTypeFlagBits::PositiveSeq,
            sequence: 1,
            payload: pcm_bytes,
            ..Message::default()
        });
        let mut resampler = None;
        let out = route_frame(&frame, &state, &mut resampler, &weak)
            .await
            .unwrap();
        assert_eq!(out, None, "cancelled turn's audio is discarded");
    }

    #[tokio::test]
    async fn route_frame_session_finished_resets_started_and_fires() {
        let (session, state) = session_and_state();
        let weak = Arc::downgrade(&session.inner);
        {
            let mut st = state.lock().await;
            st.started = true;
            st.current = Some(ActiveTask::new());
        }
        let frame = server_frame(Message {
            msg_type: MsgType::FullServerResponse,
            flag: MsgTypeFlagBits::WithEvent,
            event: EventType::SessionFinished,
            session_id: "call-uuid".into(),
            payload: b"{}".to_vec(),
            ..Message::default()
        });
        let mut resampler = None;
        route_frame(&frame, &state, &mut resampler, &weak)
            .await
            .unwrap();
        let st = state.lock().await;
        assert!(
            !st.started,
            "SessionFinished resets started so next turn reopens"
        );
        assert!(
            st.current
                .as_ref()
                .unwrap()
                .completed
                .load(std::sync::atomic::Ordering::Relaxed),
            "SessionFinished fires turn completion"
        );
    }

    #[tokio::test]
    async fn route_frame_tts_sentence_end_fires_completion() {
        let (session, state) = session_and_state();
        let weak = Arc::downgrade(&session.inner);
        {
            let mut st = state.lock().await;
            st.current = Some(ActiveTask::new());
        }
        let frame = server_frame(Message {
            msg_type: MsgType::FullServerResponse,
            flag: MsgTypeFlagBits::WithEvent,
            event: EventType::TTSSentenceEnd,
            session_id: "call-uuid".into(),
            payload: b"{}".to_vec(),
            ..Message::default()
        });
        let mut resampler = None;
        route_frame(&frame, &state, &mut resampler, &weak)
            .await
            .unwrap();
        let st = state.lock().await;
        assert!(
            st.current
                .as_ref()
                .unwrap()
                .completed
                .load(std::sync::atomic::Ordering::Relaxed),
            "TTSSentenceEnd fires turn completion (best-effort early complete)"
        );
    }
}
