//! Endpoint I/O callbacks + per-call UDP media state for fswtch_unicast.
//!
//! FreeSWITCH drives this module as an **endpoint interface** (not an
//! application). The [`fswtch::EndpointIoRoutines`] trait implementation
//! [`FswtchUnicast`] supplies `read_frame`, `write_frame`, `kill_channel`, and
//! `outgoing_channel` safe methods.
//!
//! # Media flow
//!
//! - `outgoing_channel` parses `fswtch_unicast/<ip>:<port>` from the caller
//!   profile's `destination_number`, creates a B-leg session, and starts a
//!   per-call UDP socket bound to a dynamic local port.
//! - `write_frame` receives caller audio (i16 PCM) and forwards it to a tokio
//!   UDP send task via an async channel.
//! - `read_frame` drains UDP-received PCM from an async channel into the frame
//!   buffer; missing samples are filled with silence.
//! - `kill_channel` removes the per-call state, aborting the UDP tasks. A
//!   background [`reap_loop`] reclaims entries whose session was destroyed
//!   without `kill_channel(SIG_KILL)` ever firing.
//!
//! The UDP payload is **raw little-endian i16 PCM** with no framing — one UDP
//! socket per call, raw PCM in both directions.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use fswtch::{
    CallerProfile, EndpointInterfaceRef, EndpointIoRoutines, Frame, FrameMut, OriginateFlag,
    OutgoingResult, SUCCESS, Session, Status, request_session,
};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// Default read/write codec parameters: L16, 8 kHz, 20 ms, mono.
const SAMPLE_RATE: u32 = 8000;
const PACKET_MS: u32 = 20;
const CHANNELS: u32 = 1;

/// Maximum UDP packet payload in bytes. Covers L16 at 48 kHz / 20 ms stereo
/// with headroom; for 8 kHz mono a frame is only 320 bytes.
const UDP_BUF_BYTES: usize = 2048;

/// Channel capacity between the FS media thread and the tokio UDP tasks,
/// measured in frames. 256 frames ≈ 5 s at 20 ms / frame.
const FRAME_CHANNEL_CAP: usize = 256;

/// Soft cap on the recv staging buffer, in samples. Bounds memory under
/// sustained stalls where `read_frame` is not driven (the mpsc channel already
/// caps incoming frames at `FRAME_CHANNEL_CAP`; this is a defensive ceiling on
/// the drained buffer). Excess is dropped oldest-first. 81_920 samples ≈ 5.1 s
/// at 8 kHz mono.
const RECV_BUFFER_CAP_SAMPLES: usize = 81_920;

/// How often the orphan-call [`reap_loop`] scans [`CALLS`] for entries whose
/// FreeSWITCH session has been destroyed without `kill_channel(SIG_KILL)`.
const REAP_INTERVAL: Duration = Duration::from_secs(10);

/// Global registry: call UUID → per-call UDP/media state.
pub static CALLS: std::sync::LazyLock<DashMap<String, CallState>> =
    std::sync::LazyLock::new(DashMap::new);

/// Per-call media state.
pub struct CallState {
    pub uuid: String,
    /// Remote UDP address (the peer's UDP port).
    pub remote_addr: SocketAddr,
    /// Sender from FS media thread → tokio send task.
    pub send_tx: mpsc::Sender<Vec<i16>>,
    /// Receiver from tokio recv task → FS media thread.
    pub recv_rx: mpsc::Receiver<Vec<i16>>,
    /// Staging buffer for samples that arrived before `read_frame` needs them.
    pub recv_buffer: VecDeque<i16>,
    /// UDP receive task handle.
    pub recv_task: JoinHandle<()>,
    /// UDP send task handle.
    pub send_task: JoinHandle<()>,
}

impl CallState {
    /// Create per-call UDP state and spawn tokio send/recv tasks.
    pub fn new(uuid: String, remote_addr: SocketAddr) -> anyhow::Result<Self> {
        // `outgoing_channel` calls us on FreeSWITCH's synchronous originate
        // thread, which has NO tokio context — and `tokio::net::UdpSocket`
        // constructors panic without a reactor ("there is no reactor
        // running"). Enter the module runtime via `runtime::block_on` for the
        // bind; the async constructor also sets the socket non-blocking
        // internally. (Regression history: `UdpSocket::from_std` here panicked
        // on every call setup; `call_state_for_test` bypassing `new` hid it.)
        let socket = crate::runtime::block_on(UdpSocket::bind("0.0.0.0:0"))
            .ok_or_else(|| anyhow::anyhow!("tokio runtime not started"))?
            .map_err(|e| anyhow::anyhow!("UDP bind failed: {e}"))?;
        let socket = Arc::new(socket);

        let (send_tx, send_rx) = mpsc::channel::<Vec<i16>>(FRAME_CHANNEL_CAP);
        let (recv_tx, recv_rx) = mpsc::channel::<Vec<i16>>(FRAME_CHANNEL_CAP);

        let recv_socket = socket.clone();
        let recv_task = crate::runtime::spawn(recv_loop(recv_socket, remote_addr, recv_tx))
            .ok_or_else(|| anyhow::anyhow!("tokio runtime not started"))?;
        let send_task = crate::runtime::spawn(send_loop(socket, remote_addr, send_rx))
            .ok_or_else(|| anyhow::anyhow!("tokio runtime not started"))?;

        Ok(Self {
            uuid,
            remote_addr,
            send_tx,
            recv_rx,
            recv_buffer: VecDeque::new(),
            recv_task,
            send_task,
        })
    }
}

impl Drop for CallState {
    fn drop(&mut self) {
        self.recv_task.abort();
        self.send_task.abort();
        tracing::debug!("CallState dropped for {}", self.uuid);
    }
}

/// Tokio task: read raw PCM from UDP and forward i16 samples to the FS media
/// thread via `tx`. Only packets from `remote` are accepted: raw UDP has no
/// authentication, so without the source check any host that can route to our
/// port could inject audio into the call.
///
/// Socket errors are logged (throttled) and the task keeps running. On Linux
/// an ICMP port-unreachable from the peer surfaces as `ECONNREFUSED` on the
/// next socket operation even for unconnected UDP — a transient peer restart
/// must not silence the call's media permanently.
async fn recv_loop(socket: Arc<UdpSocket>, remote: SocketAddr, tx: mpsc::Sender<Vec<i16>>) {
    let mut buf = vec![0u8; UDP_BUF_BYTES];
    let mut consecutive_errors = 0u64;
    loop {
        match socket.recv_from(&mut buf).await {
            Ok((n, addr)) => {
                if addr != remote {
                    tracing::trace!(
                        "recv_loop: dropping {n} bytes from unexpected source {addr} \
                         (peer is {remote})"
                    );
                    continue;
                }
                consecutive_errors = 0;
                let samples = bytes_to_i16(&buf[..n]);
                if tx.send(samples).await.is_err() {
                    // Receiver dropped (call ended).
                    break;
                }
            }
            Err(e) => {
                consecutive_errors += 1;
                if consecutive_errors == 1 || consecutive_errors.is_multiple_of(50) {
                    tracing::warn!("UDP recv error (consecutive: {consecutive_errors}): {e}");
                }
                // Back off so a persistent error cannot spin the loop hot.
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }
}

/// Tokio task: receive i16 PCM from the FS media thread and send raw LE bytes
/// to `remote`. Reuses a single send buffer to avoid per-frame allocation.
/// Socket errors are logged (throttled) and the task keeps running — a
/// transient error must not mute the caller for the rest of the call. The
/// loop is rate-limited by the channel, so no backoff is needed here.
async fn send_loop(socket: Arc<UdpSocket>, remote: SocketAddr, mut rx: mpsc::Receiver<Vec<i16>>) {
    let mut buf = Vec::<u8>::with_capacity(UDP_BUF_BYTES);
    let mut consecutive_errors = 0u64;
    while let Some(samples) = rx.recv().await {
        write_i16_le(&samples, &mut buf);
        match socket.send_to(&buf, remote).await {
            Ok(_) => consecutive_errors = 0,
            Err(e) => {
                consecutive_errors += 1;
                if consecutive_errors == 1 || consecutive_errors.is_multiple_of(50) {
                    tracing::warn!("UDP send error (consecutive: {consecutive_errors}): {e}");
                }
            }
        }
    }
}

/// Encode PCM samples as little-endian bytes into `out` (cleared first). Used by
/// [`send_loop`] with a reused buffer; it is the inverse of [`bytes_to_i16`].
fn write_i16_le(samples: &[i16], out: &mut Vec<u8>) {
    out.clear();
    out.reserve(samples.len() * 2);
    for &sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
}

/// Convert little-endian bytes from UDP to PCM samples. Odd trailing byte is
/// ignored.
fn bytes_to_i16(bytes: &[u8]) -> Vec<i16> {
    bytes
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect()
}

/// Drain queued recv chunks into `state.recv_buffer`, then copy `buf.len()`
/// samples into `buf`. Missing samples are zero-filled (silence). This is the
/// pure, FreeSWITCH-free core of [`FswtchUnicast::read_frame`], extracted so it
/// can be exercised without a live session.
fn stage_and_fill(state: &mut CallState, buf: &mut [i16]) {
    // Move all available received chunks into the staging buffer.
    while let Ok(chunk) = state.recv_rx.try_recv() {
        state.recv_buffer.extend(chunk);
    }
    // Defensive cap: drop oldest if a sustained stall let it grow past the
    // ceiling (the mpsc cap alone bounds this only indirectly).
    while state.recv_buffer.len() > RECV_BUFFER_CAP_SAMPLES {
        state.recv_buffer.pop_front();
    }

    let available = state.recv_buffer.len().min(buf.len());
    for slot in buf.iter_mut().take(available) {
        *slot = state.recv_buffer.pop_front().unwrap_or(0);
    }
    for slot in &mut buf[available..] {
        *slot = 0;
    }
}

/// Forward caller-supplied samples to the tokio UDP send task. This is the pure
/// core of [`FswtchUnicast::write_frame`]; logs and drops on `Full`/`Closed`.
fn try_enqueue(state: &CallState, uuid: &str, samples: Vec<i16>) {
    if let Err(e) = state.send_tx.try_send(samples) {
        match e {
            mpsc::error::TrySendError::Full(_) => {
                tracing::warn!("write_frame: send channel full for {uuid}, dropping frame");
            }
            mpsc::error::TrySendError::Closed(_) => {
                tracing::debug!("write_frame: send channel closed for {uuid}");
            }
        }
    }
}

/// Background reaper: drops [`CallState`] entries whose FreeSWITCH session has
/// been destroyed out from under us — teardown paths where
/// `kill_channel(SIG_KILL)` never fires. Runs on the module tokio runtime every
/// [`REAP_INTERVAL`]; in normal operation `kill_channel` handles teardown and
/// the reaper is a no-op.
///
/// `SessionGuard::locate` read-locks the looked-up session, so each probe runs
/// on the blocking pool to avoid stalling the async worker (and thus the media
/// thread's mpsc channels).
async fn reap_loop() {
    let mut interval = tokio::time::interval(REAP_INTERVAL);
    loop {
        interval.tick().await;
        let uuids: Vec<String> = CALLS.iter().map(|entry| entry.key().clone()).collect();
        for uuid in uuids {
            let probe = uuid.clone();
            let gone = tokio::task::spawn_blocking(move || {
                matches!(fswtch::SessionGuard::locate(&probe), Ok(None))
            })
            .await
            .unwrap_or(false);
            if gone && let Some((_, _state)) = CALLS.remove(&uuid) {
                tracing::info!("reaper: removed orphaned call state for {uuid}");
                // `_state` drops at block end → aborts tasks, closes socket.
            }
        }
    }
}

/// Spawn the orphan-call [`reap_loop`] on the module tokio runtime. Returns
/// `None` (with a log) if the runtime isn't started — the task self-cancels
/// when the runtime stops.
pub(crate) fn spawn_reaper() -> Option<JoinHandle<()>> {
    crate::runtime::spawn(reap_loop())
}

/// The `fswtch_unicast` endpoint: a unit-struct implementing
/// [`fswtch::EndpointIoRoutines`]. Per-call state lives in the global [`CALLS`]
/// map keyed by session UUID (endpoints receive no `user_data`).
pub struct FswtchUnicast;

impl EndpointIoRoutines for FswtchUnicast {
    const NAME: &'static str = "fswtch_unicast";

    /// Create the B leg when FreeSWITCH bridges to `fswtch_unicast/<ip>:<port>`.
    ///
    /// Parses the remote UDP address from `destination_number`, creates a new
    /// session on this endpoint, sets up L16 8 kHz mono codecs, and starts the
    /// per-call UDP socket + tasks.
    ///
    /// If `CallState::new` (UDP bind / task spawn) fails, the B-leg is still
    /// handed to FreeSWITCH via `success` with **degraded media** (`read_frame`
    /// emits silence, `write_frame` drops — no entry in [`CALLS`]). Refusing
    /// would be the intuitive choice, but `fswtch` exposes no
    /// `switch_core_session_destroy`, and `Session` has no `Drop`/refcount
    /// decrement — so refusing without handing the session to FS would strand
    /// it: `hangup` only sets the cause, it does not run the state machine to
    /// `CS_DESTROY`, leaking the session. Success lets FS launch the state
    /// machine and tear the B-leg down normally when the call ends. This
    /// matches the `ai-agent-seat` sibling. The failure itself (UDP bind to
    /// `0.0.0.0:0`) is near-impossible except under socket/port exhaustion.
    fn outgoing_channel(
        _session: Option<&Session>,
        caller_profile: Option<CallerProfile>,
        endpoint: &EndpointInterfaceRef,
        flags: OriginateFlag,
    ) -> OutgoingResult {
        let Some(profile) = caller_profile else {
            tracing::error!("outgoing_channel: missing caller profile");
            return OutgoingResult::refused();
        };

        let dest = match profile.field("destination_number") {
            Ok(Some(d)) => d,
            Ok(None) => {
                tracing::error!("outgoing_channel: destination_number not set");
                return OutgoingResult::refused();
            }
            Err(e) => {
                tracing::error!("outgoing_channel: read destination_number failed: {e}");
                return OutgoingResult::refused();
            }
        };

        // Strip the endpoint prefix; tolerate bare addresses for robustness.
        let addr_str = dest.strip_prefix("fswtch_unicast/").unwrap_or(&dest);
        let remote_addr: SocketAddr = match addr_str.parse() {
            Ok(a) => a,
            Err(e) => {
                tracing::error!("outgoing_channel: invalid UDP address '{addr_str}': {e}");
                return OutgoingResult::refused();
            }
        };

        let Some(new_session) = request_session(endpoint, fswtch::CallDirection::OUTBOUND, flags)
        else {
            tracing::error!("outgoing_channel: session request failed");
            return OutgoingResult::refused();
        };

        let Some(channel) = new_session.channel() else {
            tracing::error!("outgoing_channel: get_channel returned null");
            // No channel to hang up through; FS will reclaim the session.
            return OutgoingResult::refused();
        };

        channel.set_caller_profile(&profile);
        let _ = channel.set_name("fswtch_unicast");
        let _ = channel.mark_answered();
        channel.set_audio_flag();

        if let Err(e) = new_session.init_read_codec("L16", SAMPLE_RATE, PACKET_MS, CHANNELS) {
            tracing::warn!("outgoing_channel: init_read_codec failed: {e}");
        }
        if let Err(e) = new_session.init_write_codec("L16", SAMPLE_RATE, PACKET_MS, CHANNELS) {
            tracing::warn!("outgoing_channel: init_write_codec failed: {e}");
        }

        // Drive the state machine out of CS_NEW into the media-exchange state.
        channel.set_state(fswtch::ChannelState::CONSUME_MEDIA);

        let uuid = channel.uuid().unwrap_or_default();
        if !uuid.is_empty() {
            match CallState::new(uuid.clone(), remote_addr) {
                Ok(state) => {
                    tracing::info!("outgoing_channel: created session {uuid} remote={remote_addr}");
                    CALLS.insert(uuid, state);
                }
                Err(e) => {
                    // See the doc comment above: we intentionally do NOT refuse
                    // here. Hand the session to FS (success below) so its state
                    // machine can tear it down normally; read_frame/write_frame
                    // degrade to silence/drop with no CALLS entry. Refusing
                    // would leak the unlaunched session (no destroy API).
                    tracing::error!(
                        "outgoing_channel: CallState::new failed for {uuid}: {e} — \
                         session handed to FS with degraded (silent) media"
                    );
                }
            }
        }

        OutgoingResult::success(new_session)
    }

    /// `write_frame`: FreeSWITCH writes the CALLER'S audio TO this endpoint.
    ///
    /// Forward the i16 PCM samples to the tokio UDP send task.
    fn write_frame(session: &Session, frame: &Frame) -> Status {
        let Some(uuid) = session.channel().and_then(|c| c.uuid()) else {
            return SUCCESS;
        };

        let Some(samples) = frame.pcm_i16() else {
            return SUCCESS;
        };
        if samples.is_empty() {
            return SUCCESS;
        }

        let Some(state) = CALLS.get(&uuid) else {
            return SUCCESS;
        };

        // NOTE: `to_vec()` is required — the channel carries owned `Vec<i16>`
        // consumed asynchronously by `send_loop` after this callback returns;
        // `pcm_i16()` only borrows the frame for the call's duration. A future
        // fswtch API exposing a borrowed session UUID (avoiding the per-frame
        // `channel().uuid()` allocation) would remove the remaining allocations
        // on this path; out of scope here.
        try_enqueue(&state, &uuid, samples.to_vec());

        SUCCESS
    }

    /// `read_frame`: FreeSWITCH reads audio FROM this endpoint.
    ///
    /// Drain any UDP-received samples into `recv_buffer`, then copy the needed
    /// number of samples into the frame. Missing samples are zero-filled
    /// (silence).
    fn read_frame(session: &Session, frame: &mut FrameMut) -> Status {
        let Some(uuid) = session.channel().and_then(|c| c.uuid()) else {
            return SUCCESS;
        };

        let Some(buf) = frame.pcm_i16_output() else {
            return SUCCESS;
        };

        let Some(mut state) = CALLS.get_mut(&uuid) else {
            // No per-call state (e.g. CallState::new failed earlier): emit
            // silence rather than uninitialized media.
            for s in buf {
                *s = 0;
            }
            return SUCCESS;
        };

        stage_and_fill(&mut state, buf);

        SUCCESS
    }

    /// `kill_channel`: call ended — remove the [`CallState`] from [`CALLS`].
    fn kill_channel(session: &Session, sig: i32) -> Status {
        // FreeSWITCH's `switch_signal_t` (see `switch_types.h`): NONE=0, KILL=1,
        // XFER=2, BREAK=3. Only KILL tears the call down; XFER/BREAK are
        // media-control signals and must not destroy state. `fswtch` does not
        // yet expose a named `SwitchSig`, so this mirrors the header.
        const SIG_KILL: i32 = 1;
        if sig != SIG_KILL {
            tracing::trace!("kill_channel sig={sig} (non-KILL; keeping call state)");
            return SUCCESS;
        }

        let Some(uuid) = session.channel().and_then(|c| c.uuid()) else {
            return SUCCESS;
        };

        if CALLS.remove(&uuid).is_some() {
            tracing::info!("kill_channel: removed call state for {uuid}");
            // The removed `CallState` is dropped at the end of this statement,
            // aborting the tokio tasks and closing the UDP socket.
        }

        SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A [`CallState`] with live mpsc channels but no real UDP socket or
    /// runtime tasks, so unit tests can exercise [`stage_and_fill`] and
    /// [`try_enqueue`] without FreeSWITCH or the module runtime. Also returns
    /// the `recv_tx` (to inject received samples) and `send_rx` (to observe
    /// enqueued samples). Must be called from a tokio runtime context (the
    /// tests below are `#[tokio::test]`).
    fn call_state_for_test() -> (CallState, mpsc::Sender<Vec<i16>>, mpsc::Receiver<Vec<i16>>) {
        let (send_tx, send_rx) = mpsc::channel::<Vec<i16>>(FRAME_CHANNEL_CAP);
        let (recv_tx, recv_rx) = mpsc::channel::<Vec<i16>>(FRAME_CHANNEL_CAP);
        // `tokio::spawn` is available because callers run inside `#[tokio::test]`.
        // The handles complete immediately; `Drop` aborts them harmlessly.
        let recv_task = tokio::spawn(async {});
        let send_task = tokio::spawn(async {});
        let state = CallState {
            uuid: String::from("test-uuid"),
            remote_addr: "127.0.0.1:0".parse().unwrap(),
            send_tx,
            recv_rx,
            recv_buffer: VecDeque::new(),
            recv_task,
            send_task,
        };
        (state, recv_tx, send_rx)
    }

    #[test]
    fn i16_bytes_roundtrip() {
        let samples: Vec<i16> = vec![0, 1, -1, i16::MAX, i16::MIN, 32767, -32768];
        let mut bytes = Vec::new();
        write_i16_le(&samples, &mut bytes);
        let decoded = bytes_to_i16(&bytes);
        assert_eq!(decoded, samples);
    }

    #[test]
    fn bytes_to_i16_ignores_trailing_odd_byte() {
        let bytes = vec![0x01, 0x00, 0xFF]; // 1 i16 sample + 1 trailing byte
        let decoded = bytes_to_i16(&bytes);
        assert_eq!(decoded, vec![1]);
    }

    #[tokio::test]
    async fn stage_and_fill_drains_chunks_and_zero_fills_tail() {
        let (mut state, recv_tx, _send_rx) = call_state_for_test();
        recv_tx.send(vec![1, 2, 3]).await.unwrap();
        recv_tx.send(vec![4, 5]).await.unwrap();

        let mut buf = vec![0i16; 10];
        stage_and_fill(&mut state, &mut buf);

        // First 5 samples come from the queued chunks in order; rest silenced.
        assert_eq!(buf, vec![1, 2, 3, 4, 5, 0, 0, 0, 0, 0]);
        // Buffer fully drained (we asked for ≥ what was queued).
        assert!(state.recv_buffer.is_empty());
    }

    #[tokio::test]
    async fn stage_and_fill_silences_when_no_input() {
        let (mut state, _recv_tx, _send_rx) = call_state_for_test();
        let mut buf = vec![1i16; 5];
        stage_and_fill(&mut state, &mut buf);
        assert_eq!(buf, vec![0, 0, 0, 0, 0]);
    }

    #[tokio::test]
    async fn stage_and_fill_preserves_leftover_for_next_frame() {
        let (mut state, recv_tx, _send_rx) = call_state_for_test();
        recv_tx.send(vec![1, 2, 3, 4, 5]).await.unwrap();

        let mut buf = vec![0i16; 2];
        stage_and_fill(&mut state, &mut buf);
        assert_eq!(buf, vec![1, 2]);
        // 3 samples remain staged for the next read_frame.
        assert_eq!(
            state.recv_buffer.iter().copied().collect::<Vec<_>>(),
            vec![3, 4, 5]
        );

        let mut buf = vec![0i16; 3];
        stage_and_fill(&mut state, &mut buf);
        assert_eq!(buf, vec![3, 4, 5]);
        assert!(state.recv_buffer.is_empty());
    }

    #[tokio::test]
    async fn try_enqueue_delivers_to_send_channel() {
        let (state, _recv_tx, mut send_rx) = call_state_for_test();
        try_enqueue(&state, "test-uuid", vec![1, -1, 2]);
        let got = tokio::time::timeout(Duration::from_secs(1), send_rx.recv())
            .await
            .expect("timed out waiting for send_rx")
            .expect("send_rx closed unexpectedly");
        assert_eq!(got, vec![1, -1, 2]);
    }

    #[tokio::test]
    async fn try_enqueue_drops_overflow_without_panicking() {
        // Bounded channel cap is FRAME_CHANNEL_CAP; overflowing must drop via
        // `Full` rather than block or panic.
        let (state, _recv_tx, _send_rx) = call_state_for_test();
        for i in 0..(FRAME_CHANNEL_CAP + 50) {
            try_enqueue(&state, "test-uuid", vec![i as i16]);
        }
    }

    #[tokio::test]
    async fn recv_loop_forwards_udp_payload_to_channel() {
        let listener = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let listener_addr = listener.local_addr().unwrap();
        let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let sender_addr = sender.local_addr().unwrap();
        let (tx, mut rx) = mpsc::channel::<Vec<i16>>(8);
        let task = tokio::spawn(recv_loop(listener, sender_addr, tx));

        // Send two LE i16 samples (1, -1) from the accepted peer address.
        sender
            .send_to(&[0x01, 0x00, 0xFF, 0xFF], listener_addr)
            .await
            .unwrap();

        let samples = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out waiting for recv_loop")
            .expect("recv channel closed");
        assert_eq!(samples, vec![1, -1]);
        task.abort();
    }

    #[tokio::test]
    async fn recv_loop_drops_packets_from_unexpected_source() {
        let listener = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let listener_addr = listener.local_addr().unwrap();
        let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let peer_addr = peer.local_addr().unwrap();
        let (tx, mut rx) = mpsc::channel::<Vec<i16>>(8);
        let task = tokio::spawn(recv_loop(listener, peer_addr, tx));

        // A stranger (different source port) and the real peer both send.
        let stranger = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        stranger
            .send_to(&[0x01, 0x00], listener_addr)
            .await
            .unwrap();
        peer.send_to(&[0x02, 0x00], listener_addr).await.unwrap();

        // Only the peer's sample (2) is forwarded.
        let samples = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out waiting for recv_loop")
            .expect("recv channel closed");
        assert_eq!(samples, vec![2]);
        // And nothing from the stranger is queued behind it.
        assert!(
            tokio::time::timeout(Duration::from_millis(100), rx.recv())
                .await
                .is_err()
        );
        task.abort();
    }

    #[tokio::test]
    async fn send_loop_emits_udp_payload_from_channel() {
        let receiver = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let receiver_addr = receiver.local_addr().unwrap();
        let sender = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let (tx, rx) = mpsc::channel::<Vec<i16>>(8);
        let task = tokio::spawn(send_loop(sender, receiver_addr, rx));

        tx.send(vec![1, -1]).await.unwrap();

        let mut buf = [0u8; 4];
        let (n, _) = tokio::time::timeout(Duration::from_secs(1), receiver.recv_from(&mut buf))
            .await
            .expect("timed out waiting for send_loop")
            .expect("recv_from failed");
        assert_eq!(n, 4);
        assert_eq!(&buf[..], &[0x01, 0x00, 0xFF, 0xFF]);
        task.abort();
    }

    /// Regression test: `CallState::new` must succeed on a thread with NO
    /// tokio runtime context — that is exactly how `outgoing_channel` calls it
    /// in production. Constructors that need a reactor (e.g. the former
    /// `UdpSocket::from_std`) panic there with "there is no reactor running",
    /// and the rest of this suite cannot catch that: `call_state_for_test`
    /// bypasses `new` and `#[tokio::test]` provides a context. Deliberately a
    /// plain `#[test]` + bare `std::thread`.
    #[test]
    fn call_state_new_works_on_bare_thread_without_tokio_context() {
        crate::runtime::start_for_test();
        let created = std::thread::spawn(|| {
            CallState::new(
                "bare-thread-uuid".to_string(),
                "127.0.0.1:9999".parse().unwrap(),
            )
        })
        .join()
        .expect("CallState::new panicked on a non-tokio thread");
        let state = created.expect("CallState::new failed on a non-tokio thread");
        // Socket bound and both UDP tasks running on the module runtime.
        assert!(!state.recv_task.is_finished());
        assert!(!state.send_task.is_finished());
        drop(state); // aborts the UDP tasks; runtime stays up for other tests
    }
}
