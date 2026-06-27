//! Volcano bidirectional WebSocket TTS client.
//!
//! ONE WebSocket connection per call; ONE session PER TURN (per `synthesize`
//! call). The connection is established at call-answer time via [`start`]
//! (idempotent + race-safe) and reused for every turn; if it breaks mid-call
//! it is released and reconnected on the next `synthesize`. If `start` was
//! never called or failed, `synthesize` lazily connects on first use.
//!
//! `session_id` and `section_id` are BOTH the call UUID (passed in). The
//! server correlates cross-turn context via `section_id`.
//!
//! # Per-turn cycle
//!
//! Each `synthesize` call does, on the EXISTING connection:
//! 1. ensure the connection is up (lazy fallback if `start` was not called)
//! 2. `start_session` — a NEW session for this turn
//! 3. `task_request(text)`
//! 4. `finish_session`
//! 5. forward `AudioOnlyServer` PCM to `tts_out` until `SessionFinished`
//!
//! # Barge-in
//!
//! `CancellationToken` cancels the CURRENT playback+forwarding only. It does
//! NOT send `CancelSession` mid-session. On cancel we stop forwarding audio to
//! `tts_out` and keep draining/discarding server audio until `SessionFinished`.
//! `finish_session` is ALWAYS sent (even on cancel) so the server cleanly
//! closes the session.

use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::{Mutex, Notify};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::handshake::client::Request;

use crate::tts_ws_codec::{EventType, Message, MsgType};

/// The Volcano bidirectional TTS WebSocket endpoint.
const ENDPOINT: &str = "wss://openspeech.bytedance.com/api/v3/tts/bidirection";

/// Command sent to the driver task that owns the socket.
enum DriverCmd {
    Send(Message),
    Shutdown,
}

/// Internal session state shared between the API surface and the driver task.
struct SessionState {
    started: bool,
    current: Option<ActiveTask>,
}

/// The active turn's forwarding target + completion signal.
struct ActiveTask {
    tts_out: tokio::sync::mpsc::Sender<Vec<i16>>,
    done: Arc<Notify>,
    cancelled: bool,
    start: std::time::Instant,
    first_chunk_emitted: std::sync::atomic::AtomicBool,
}

/// A bidirectional Volcano TTS session bound to one call.
///
/// Constructed cheaply (sync) with the call UUID; the WebSocket connect +
/// first `start_session` happen in [`start`](Self::start) (call-answer time)
/// or lazily on the first `synthesize` if `start` was not called.
/// Cloning is cheap (Arc inner).
#[derive(Clone)]
pub struct VolcanoBidirectionalSession {
    inner: Arc<SessionInner>,
}

struct SessionInner {
    api_key: String,
    resource_id: String,
    speaker: String,
    call_uuid: String,
    send_mutex: Mutex<()>,
    state: Arc<Mutex<SessionState>>,
    cmd_tx: parking_lot::Mutex<Option<tokio::sync::mpsc::Sender<DriverCmd>>>,
    driver_join: parking_lot::Mutex<Option<tokio::task::JoinHandle<()>>>,
    started_flag: std::sync::atomic::AtomicBool,
}

impl VolcanoBidirectionalSession {
    /// Build a session for the given credentials + speaker, bound to the
    /// call UUID. The UUID is used for BOTH `session_id` and `section_id`
    /// (cross-turn correlation). The connect happens later in [`start`] or
    /// lazily in [`synthesize`].
    pub fn new(api_key: String, resource_id: String, speaker: String, call_uuid: String) -> Self {
        tracing::debug!(
            "Volcano TTS session constructed: call_uuid={} (session_id=section_id=call_uuid)",
            call_uuid
        );
        Self {
            inner: Arc::new(SessionInner {
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
            }),
        }
    }

    /// Eagerly establish the WebSocket connection + open the FIRST session.
    ///
    /// Intended to be called at call-answer time. Idempotent + race-safe.
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
        if self.inner.cmd_tx.lock().is_some() {
            drop(_send_guard);
            return self.wait_for_started().await;
        }

        if let Err(e) = self.connect_and_spawn().await {
            tracing::warn!("Volcano TTS start() connect failed: {e}");
            return Err(e);
        }
        if let Err(e) = self.start_session().await {
            tracing::warn!("Volcano TTS start() first start_session failed: {e}");
            return Err(e);
        }
        self.inner
            .started_flag
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    async fn ensure_started(&self) -> Result<()> {
        if self.is_started().await {
            return Ok(());
        }
        let _send_guard = self.inner.send_mutex.lock().await;
        if self.is_started().await {
            return Ok(());
        }
        self.release_stale_driver_if_any();
        if self.inner.cmd_tx.lock().is_some() {
            return self.start_session().await;
        }
        self.connect_and_spawn().await?;
        self.start_session().await?;
        self.inner
            .started_flag
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
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

    async fn connect_and_spawn(&self) -> Result<()> {
        // rustls 0.23 needs an explicit CryptoProvider installed process-wide.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let connect_id = uuid::Uuid::new_v4().to_string();
        let uri = ENDPOINT
            .parse::<tokio_tungstenite::tungstenite::http::Uri>()
            .context("Volcano TTS endpoint parse failed")?;
        let host = uri.host().unwrap_or("openspeech.bytedance.com").to_string();

        let req = Request::builder()
            .method("GET")
            .uri(ENDPOINT)
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
        let driver = tokio::spawn(driver_loop(ws_stream, cmd_rx, state));
        *self.inner.driver_join.lock() = Some(driver);

        self.send_raw(Message::start_connection()).await?;
        Ok(())
    }

    async fn start_session(&self) -> Result<()> {
        let payload = serde_json::json!({
            "req_params": {
                "speaker": self.inner.speaker,
                "audio_params": {
                    "format": "pcm",
                    "sample_rate": 16000,
                },
                "section_id": self.inner.call_uuid,
            }
        });
        let payload_bytes = serde_json::to_vec(&payload)?;
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

    /// Send a `task_request` for `text`, then forward server audio to
    /// `tts_out` until the session ends (or `cancel` fires).
    ///
    /// Returns `true` if the turn completed normally, `false` if it was
    /// cancelled (barge-in) — the connection stays alive either way.
    pub async fn synthesize(
        &self,
        text: &str,
        cancel: tokio_util::sync::CancellationToken,
        tts_out: tokio::sync::mpsc::Sender<Vec<i16>>,
    ) -> Result<bool> {
        tracing::info!("Volcano TTS synthesize: {} chars", text.chars().count());

        self.ensure_started().await?;

        let _send_guard = self.inner.send_mutex.lock().await;

        if cancel.is_cancelled() {
            return Ok(false);
        }

        let done = Arc::new(Notify::new());
        {
            let mut st = self.inner.state.lock().await;
            st.current = Some(ActiveTask {
                tts_out: tts_out.clone(),
                done: Arc::clone(&done),
                cancelled: false,
                start: std::time::Instant::now(),
                first_chunk_emitted: std::sync::atomic::AtomicBool::new(false),
            });
        }

        let payload = serde_json::json!({
            "req_params": {
                "text": text,
                "speaker": self.inner.speaker,
                "audio_params": {
                    "format": "pcm",
                    "sample_rate": 16000,
                },
            }
        });
        let payload_bytes = serde_json::to_vec(&payload)?;
        let send_result = self
            .send_raw(Message::task_request(payload_bytes, &self.inner.call_uuid))
            .await;

        if let Err(e) = send_result {
            self.inner.state.lock().await.current = None;
            return Err(e);
        }

        let outcome = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                {
                    let mut st = self.inner.state.lock().await;
                    if let Some(t) = st.current.as_mut() {
                        t.cancelled = true;
                    }
                }
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    done.notified(),
                ).await;
                false
            }
            _ = done.notified() => true,
            _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => {
                tracing::warn!("Volcano TTS synthesize timed out (10s)");
                true
            }
        };

        self.inner.state.lock().await.current = None;
        Ok(outcome)
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
) {
    use futures::{SinkExt, StreamExt};
    let (mut sink, mut stream) = ws_stream.split();

    loop {
        tokio::select! {
            biased;
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
                    if let Err(e) = route_frame(&bytes, &state).await {
                        tracing::warn!("Volcano TTS frame route error: {e}");
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
    {
        let mut st = state.lock().await;
        st.started = false;
        if let Some(t) = st.current.as_ref() {
            t.done.notify_one();
        }
    }
    tracing::info!("Volcano TTS driver loop exiting");
}

/// Unmarshal one server frame and route it.
async fn route_frame(bytes: &[u8], state: &Mutex<SessionState>) -> Result<()> {
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
                tracing::info!("Volcano TTS SessionFinished");
                let notify = {
                    let st = state.lock().await;
                    st.current.as_ref().map(|t| Arc::clone(&t.done))
                };
                if let Some(n) = notify {
                    n.notify_one();
                }
            }
            EventType::SessionFailed | EventType::ConnectionFailed => {
                tracing::error!(
                    "Volcano TTS session/connection failed: code={}, payload={}",
                    msg.error_code,
                    String::from_utf8_lossy(&msg.payload)
                );
                let notify = {
                    let mut st = state.lock().await;
                    st.started = false;
                    st.current.as_ref().map(|t| Arc::clone(&t.done))
                };
                if let Some(n) = notify {
                    n.notify_one();
                }
            }
            EventType::TTSSentenceStart => {
                tracing::debug!("Volcano TTS TTSSentenceStart");
            }
            EventType::TTSSentenceEnd => {
                let notify = {
                    let st = state.lock().await;
                    st.current.as_ref().map(|t| Arc::clone(&t.done))
                };
                if let Some(n) = notify {
                    tracing::debug!("Volcano TTS TTSSentenceEnd — unblocking forwarder");
                    n.notify_one();
                }
            }
            _ => {
                tracing::trace!("Volcano TTS server event: {:?}", msg.event);
            }
        },
        MsgType::AudioOnlyServer => {
            let samples = parse_pcm_le(&msg.payload);

            {
                let st = state.lock().await;
                if let Some(task) = &st.current {
                    if !task
                        .first_chunk_emitted
                        .swap(true, std::sync::atomic::Ordering::Relaxed)
                    {
                        let ms = task.start.elapsed().as_millis() as f64;
                        tracing::info!(target: "pipeline", "Volcano TTS 首Chunk: {:.1}ms", ms);
                    }
                }
            }

            if !samples.is_empty() {
                let (tx, cancelled) = {
                    let st = state.lock().await;
                    match &st.current {
                        Some(t) => (Some(t.tts_out.clone()), t.cancelled),
                        None => (None, true),
                    }
                };
                if let Some(tx) = tx
                    && !cancelled
                {
                    let _ = tx.try_send(samples);
                }
            }
        }
        MsgType::Error => {
            tracing::error!(
                "Volcano TTS server Error: code={}, payload={}",
                msg.error_code,
                String::from_utf8_lossy(&msg.payload)
            );
            let notify = {
                let st = state.lock().await;
                st.current.as_ref().map(|t| Arc::clone(&t.done))
            };
            if let Some(n) = notify {
                n.notify_one();
            }
        }
        _ => {
            tracing::trace!("Volcano TTS unexpected msg_type: {:?}", msg.msg_type);
        }
    }
    Ok(())
}

/// Parse raw L16 (little-endian i16) PCM bytes into samples.
fn parse_pcm_le(bytes: &[u8]) -> Vec<i16> {
    let usable = bytes.len() - (bytes.len() % 2);
    bytes[..usable]
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect()
}

// ── Actix wrapper: TtsActor ────────────────────────────────────────────

use actix::prelude::*;
use tokio::sync::mpsc;

use crate::audio_dsp::PIPELINE_SAMPLE_RATE;
use crate::voice_core::Config;

/// Maximum number of TTS chunks buffered in the channel.
const TTS_CHANNEL_CAPACITY: usize = 64;

/// Size (in 16 kHz mono i16 samples) of each chunk pushed to the media bug.
/// 320 samples = 20 ms at 16 kHz.
const TTS_CHUNK_SAMPLES: usize = 320;

/// Signals crossing the actor → media-bug boundary for TTS playback.
#[derive(Debug)]
pub enum TtsSignal {
    Chunk(Vec<i16>),
    ClearBuffer,
}

/// Message: synthesize `text` into speech and stream the audio to the bug.
#[derive(Message)]
#[rtype(result = "()")]
pub struct SynthesizeText {
    pub text: String,
}

/// Message: clear any buffered TTS audio (barge-in).
#[derive(Message)]
#[rtype(result = "()")]
pub struct ClearTtsBuffer;

/// TTS actor — actix wrapper around [`VolcanoBidirectionalSession`].
///
/// Owns the producer end of the TTS mpsc channel. Text arrives via
/// [`SynthesizeText`], is run through the Volcano bidirectional WebSocket
/// TTS API, and the resulting 16 kHz i16 PCM is forwarded to the bug.
pub struct TtsActor {
    uuid: String,
    config: Option<Config>,
    tts_tx: mpsc::Sender<TtsSignal>,
    session: Option<VolcanoBidirectionalSession>,
}

impl TtsActor {
    pub fn new(uuid: String, config: Option<Config>) -> (Self, mpsc::Receiver<TtsSignal>) {
        let (tts_tx, tts_rx) = mpsc::channel::<TtsSignal>(TTS_CHANNEL_CAPACITY);
        let session = config.as_ref().map(|c| {
            VolcanoBidirectionalSession::new(
                c.ai.tts_api_key.clone(),
                "seed-tts-2.0".to_string(),
                c.ai.tts_endpoint.clone(),
                uuid.clone(),
            )
        });
        let actor = Self {
            uuid,
            config,
            tts_tx,
            session,
        };
        (actor, tts_rx)
    }

    pub fn with_sender(
        uuid: String,
        config: Option<Config>,
        tts_tx: mpsc::Sender<TtsSignal>,
    ) -> Self {
        let session = config.as_ref().map(|c| {
            VolcanoBidirectionalSession::new(
                c.ai.tts_api_key.clone(),
                "seed-tts-2.0".to_string(),
                c.ai.tts_endpoint.clone(),
                uuid.clone(),
            )
        });
        Self {
            uuid,
            config,
            tts_tx,
            session,
        }
    }
}

impl Actor for TtsActor {
    type Context = actix::Context<Self>;

    fn started(&mut self, ctx: &mut Self::Context) {
        tracing::info!("TtsActor started for session {}", self.uuid);
        if let Some(session) = &self.session {
            let session = session.clone();
            ctx.spawn(
                async move {
                    if let Err(e) = session.start().await {
                        tracing::warn!("TtsActor: Volcano TTS start failed: {e}");
                    }
                }
                .into_actor(self),
            );
        }
    }

    fn stopped(&mut self, _ctx: &mut Self::Context) {
        tracing::info!("TtsActor stopped for session {}", self.uuid);
    }
}

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

        let tts_tx = self.tts_tx.clone();
        let session = self.session.clone();
        let uuid_for_task = uuid.clone();

        ctx.spawn(
            async move {
                if let Some(session) = &session {
                    let cancel = tokio_util::sync::CancellationToken::new();
                    // Bridge: VolcanoBidirectionalSession sends Vec<i16> audio;
                    // we wrap each chunk into TtsSignal::Chunk for the media bug.
                    let (raw_tx, mut raw_rx) =
                        tokio::sync::mpsc::channel::<Vec<i16>>(TTS_CHANNEL_CAPACITY);
                    let tts_tx_bridge = tts_tx.clone();
                    // Forward raw audio → TtsSignal::Chunk in 320-sample chunks.
                    tokio::spawn(async move {
                        while let Some(audio) = raw_rx.recv().await {
                            let mut start = 0;
                            while start < audio.len() {
                                let end = (start + TTS_CHUNK_SAMPLES).min(audio.len());
                                let chunk = audio[start..end].to_vec();
                                if tts_tx_bridge.send(TtsSignal::Chunk(chunk)).await.is_err() {
                                    return;
                                }
                                start = end;
                            }
                        }
                    });
                    match session.synthesize(&msg.text, cancel, raw_tx).await {
                        Ok(completed) => {
                            tracing::info!(
                                "TTS synthesize completed for session {} (completed={})",
                                uuid_for_task,
                                completed
                            );
                        }
                        Err(e) => {
                            tracing::error!(
                                "TTS synthesize failed for session {}: {}",
                                uuid_for_task,
                                e
                            );
                        }
                    }
                } else {
                    tracing::warn!(
                        "TTS session not configured for session {}; skipping synthesis",
                        uuid_for_task
                    );
                }
            }
            .into_actor(self),
        );
    }
}

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
pub fn tts_sample_rate() -> u32 {
    PIPELINE_SAMPLE_RATE
}
