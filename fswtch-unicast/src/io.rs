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
//! - `kill_channel` removes the per-call state, aborting the UDP tasks.
//!
//! The UDP payload is **raw little-endian i16 PCM** with no framing — one UDP
//! socket per call, raw PCM in both directions.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;

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
        // Bind a dynamic local UDP port. We create a std socket first because
        // `outgoing_channel` runs on the synchronous FS media/origination thread,
        // not inside a tokio runtime context.
        let std_socket = std::net::UdpSocket::bind("0.0.0.0:0")
            .map_err(|e| anyhow::anyhow!("UDP bind failed: {e}"))?;
        std_socket
            .set_nonblocking(true)
            .map_err(|e| anyhow::anyhow!("set_nonblocking failed: {e}"))?;
        let socket = UdpSocket::from_std(std_socket)
            .map_err(|e| anyhow::anyhow!("tokio UdpSocket from std failed: {e}"))?;
        let socket = Arc::new(socket);

        let (send_tx, send_rx) = mpsc::channel::<Vec<i16>>(FRAME_CHANNEL_CAP);
        let (recv_tx, recv_rx) = mpsc::channel::<Vec<i16>>(FRAME_CHANNEL_CAP);

        let recv_socket = socket.clone();
        let recv_task = crate::runtime::spawn(recv_loop(recv_socket, recv_tx))
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
/// thread via `tx`.
async fn recv_loop(socket: Arc<UdpSocket>, tx: mpsc::Sender<Vec<i16>>) {
    let mut buf = vec![0u8; UDP_BUF_BYTES];
    loop {
        match socket.recv_from(&mut buf).await {
            Ok((n, _addr)) => {
                let samples = bytes_to_i16(&buf[..n]);
                if tx.send(samples).await.is_err() {
                    // Receiver dropped (call ended).
                    break;
                }
            }
            Err(e) => {
                tracing::error!("UDP recv error: {e}");
                break;
            }
        }
    }
}

/// Tokio task: receive i16 PCM from the FS media thread and send raw LE bytes
/// to `remote`.
async fn send_loop(socket: Arc<UdpSocket>, remote: SocketAddr, mut rx: mpsc::Receiver<Vec<i16>>) {
    while let Some(samples) = rx.recv().await {
        let bytes = i16_to_bytes(&samples);
        if let Err(e) = socket.send_to(&bytes, remote).await {
            tracing::error!("UDP send error: {e}");
            break;
        }
    }
}

/// Convert PCM samples to little-endian bytes for UDP transmission.
fn i16_to_bytes(samples: &[i16]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for &sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

/// Convert little-endian bytes from UDP to PCM samples. Odd trailing byte is
/// ignored.
fn bytes_to_i16(bytes: &[u8]) -> Vec<i16> {
    bytes
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect()
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
                    tracing::error!("outgoing_channel: CallState::new failed: {e}");
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

        let samples = match frame.pcm_i16() {
            Some(s) if !s.is_empty() => s,
            _ => return SUCCESS,
        };

        let Some(state) = CALLS.get(&uuid) else {
            return SUCCESS;
        };

        if let Err(e) = state.send_tx.try_send(samples.to_vec()) {
            match e {
                mpsc::error::TrySendError::Full(_) => {
                    tracing::warn!("write_frame: send channel full for {uuid}, dropping frame");
                }
                mpsc::error::TrySendError::Closed(_) => {
                    tracing::debug!("write_frame: send channel closed for {uuid}");
                }
            }
        }

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
            for s in buf {
                *s = 0;
            }
            return SUCCESS;
        };

        // Move all available received chunks into the staging buffer.
        while let Ok(chunk) = state.recv_rx.try_recv() {
            state.recv_buffer.extend(chunk);
        }

        let needed = buf.len();
        let available = state.recv_buffer.len().min(needed);

        for s in buf.iter_mut().take(available) {
            *s = state.recv_buffer.pop_front().unwrap_or(0);
        }
        for s in &mut buf[available..] {
            *s = 0;
        }

        SUCCESS
    }

    /// `kill_channel`: call ended — remove the [`CallState`] from [`CALLS`].
    fn kill_channel(session: &Session, sig: i32) -> Status {
        // Only SWITCH_SIG_KILL (1) tears the call down. BREAK/XFER are media
        // control signals and must not destroy state.
        const SIG_KILL: i32 = 1;
        if sig != SIG_KILL {
            tracing::trace!("kill_channel sig={sig} (non-KILL; keeping call state)");
            return SUCCESS;
        }

        let Some(uuid) = session.channel().and_then(|c| c.uuid()) else {
            return SUCCESS;
        };

        if let Some((_, state)) = CALLS.remove(&uuid) {
            // Dropping CallState aborts the tokio tasks and closes the socket.
            drop(state);
            tracing::info!("kill_channel: removed call state for {uuid}");
        }

        SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn i16_bytes_roundtrip() {
        let samples: Vec<i16> = vec![0, 1, -1, i16::MAX, i16::MIN, 32767, -32768];
        let bytes = i16_to_bytes(&samples);
        let decoded = bytes_to_i16(&bytes);
        assert_eq!(decoded, samples);
    }

    #[test]
    fn bytes_to_i16_ignores_trailing_odd_byte() {
        let bytes = vec![0x01, 0x00, 0xFF]; // 1 i16 sample + 1 trailing byte
        let decoded = bytes_to_i16(&bytes);
        assert_eq!(decoded, vec![1]);
    }
}
