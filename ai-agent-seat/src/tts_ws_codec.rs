//! Volcano bidirectional TTS binary message codec.
//!
//! Rust port of the Python `protocols_.py` `Message` class (marshal/unmarshal),
//! including `MsgType`, `MsgTypeFlagBits`, `EventType`, etc. The wire format
//! is a 4-byte fixed header followed by variable-length fields whose presence
//! and order depend on `msg_type` / `flag` / `event` exactly as in the
//! Python reference.
//!
//! Header layout (when `header_size == HeaderSize4`, i.e. 4-byte header):
//! ```text
//! byte 0: (version << 4) | header_size
//! byte 1: (msg_type << 4) | flag
//! byte 2: (serialization << 4) | compression
//! byte 3: 0 (padding)
//! ```
//! The writer/reader order below mirrors `_get_writers` / `_get_readers`
//! verbatim — do not reorder.
//!
//! No external dependencies: pure `std`.

// ── Enums ──────────────────────────────────────────────────────────────

/// Message type enumeration (high nibble of header byte 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum MsgType {
    Invalid = 0b0000,
    FullClientRequest = 0b0001,
    AudioOnlyClient = 0b0010,
    FullServerResponse = 0b1001,
    AudioOnlyServer = 0b1011,
    FrontEndResultServer = 0b1100,
    Error = 0b1111,
}

impl MsgType {
    /// Raw discriminant value (matches the Python `IntEnum` value).
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    /// Parse from a raw nibble value. Returns `None` for unknown values
    /// (mirrors Python `MsgType(value)` raising `ValueError`).
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0b0000 => Some(MsgType::Invalid),
            0b0001 => Some(MsgType::FullClientRequest),
            0b0010 => Some(MsgType::AudioOnlyClient),
            0b1001 => Some(MsgType::FullServerResponse),
            0b1011 => Some(MsgType::AudioOnlyServer),
            0b1100 => Some(MsgType::FrontEndResultServer),
            0b1111 => Some(MsgType::Error),
            _ => None,
        }
    }
}

impl Default for MsgType {
    /// Python default: `type=Invalid`.
    fn default() -> Self {
        MsgType::Invalid
    }
}

/// Message-type flag bits (low nibble of header byte 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum MsgTypeFlagBits {
    /// Non-terminal packet with no sequence.
    NoSeq = 0,
    /// Non-terminal packet with sequence > 0.
    PositiveSeq = 0b1,
    /// Last packet with no sequence.
    LastNoSeq = 0b10,
    /// Last packet with sequence < 0.
    NegativeSeq = 0b11,
    /// Payload contains an event number (int32).
    WithEvent = 0b100,
}

impl MsgTypeFlagBits {
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(MsgTypeFlagBits::NoSeq),
            0b1 => Some(MsgTypeFlagBits::PositiveSeq),
            0b10 => Some(MsgTypeFlagBits::LastNoSeq),
            0b11 => Some(MsgTypeFlagBits::NegativeSeq),
            0b100 => Some(MsgTypeFlagBits::WithEvent),
            _ => None,
        }
    }
}

impl Default for MsgTypeFlagBits {
    /// Python default: `flag=NoSeq`.
    fn default() -> Self {
        MsgTypeFlagBits::NoSeq
    }
}

/// Version bits (high nibble of header byte 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum VersionBits {
    #[default]
    Version1 = 1,
    Version2 = 2,
    Version3 = 3,
    Version4 = 4,
}

/// Header-size bits (low nibble of header byte 0); value `n` means a
/// `4 * n` byte fixed header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum HeaderSizeBits {
    #[default]
    HeaderSize4 = 1,
    HeaderSize8 = 2,
    HeaderSize12 = 3,
    HeaderSize16 = 4,
}

/// Serialization method (high nibble of header byte 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum SerializationBits {
    Raw = 0,
    #[default]
    JSON = 0b1,
    Thrift = 0b11,
    Custom = 0b1111,
}

/// Compression method (low nibble of header byte 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum CompressionBits {
    #[default]
    None = 0,
    Gzip = 0b1,
    Custom = 0b1111,
}

/// Event type enumeration. Values are `i32` to match the Python `IntEnum`
/// and the on-wire `>i` (signed 32-bit big-endian) encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(i32)]
pub enum EventType {
    #[default]
    None = 0,

    // 1 ~ 49 Upstream Connection events
    StartConnection = 1,
    FinishConnection = 2,

    // 50 ~ 99 Downstream Connection events
    ConnectionStarted = 50,
    ConnectionFailed = 51,
    ConnectionFinished = 52,

    // 100 ~ 149 Upstream Session events
    StartSession = 100,
    CancelSession = 101,
    FinishSession = 102,

    // 150 ~ 199 Downstream Session events
    SessionStarted = 150,
    SessionCanceled = 151,
    SessionFinished = 152,
    SessionFailed = 153,
    UsageResponse = 154,

    // 200 ~ 249 Upstream general events
    TaskRequest = 200,
    UpdateConfig = 201,

    // 250 ~ 299 Downstream general events
    AudioMuted = 250,

    // 300 ~ 349 Upstream TTS events
    SayHello = 300,

    // 350 ~ 399 Downstream TTS events
    TTSSentenceStart = 350,
    TTSSentenceEnd = 351,
    TTSResponse = 352,
    TTSSubtitle = 364,
    TTSEnded = 359,
    PodcastRoundStart = 360,
    PodcastRoundResponse = 361,
    PodcastRoundEnd = 362,

    // 450 ~ 499 Downstream ASR events
    ASRInfo = 450,
    ASRResponse = 451,
    ASREnded = 459,

    // 500 ~ 549 Upstream dialogue events
    ChatTTSText = 500,

    // 550 ~ 599 Downstream dialogue events
    ChatResponse = 550,
    ChatEnded = 559,

    // 650 ~ 699 Downstream subtitle events
    SourceSubtitleStart = 650,
    SourceSubtitleResponse = 651,
    SourceSubtitleEnd = 652,
    TranslationSubtitleStart = 653,
    TranslationSubtitleResponse = 654,
    TranslationSubtitleEnd = 655,
}

impl EventType {
    pub fn as_i32(self) -> i32 {
        self as i32
    }

    /// Parse from a raw signed value. Mirrors the Python fallback: unknown
    /// values are tolerated by the reader (the raw int is preserved on the
    /// `Message` via a separate path), but [`EventType`] itself only holds
    /// known variants.
    pub fn from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(EventType::None),
            1 => Some(EventType::StartConnection),
            2 => Some(EventType::FinishConnection),
            50 => Some(EventType::ConnectionStarted),
            51 => Some(EventType::ConnectionFailed),
            52 => Some(EventType::ConnectionFinished),
            100 => Some(EventType::StartSession),
            101 => Some(EventType::CancelSession),
            102 => Some(EventType::FinishSession),
            150 => Some(EventType::SessionStarted),
            151 => Some(EventType::SessionCanceled),
            152 => Some(EventType::SessionFinished),
            153 => Some(EventType::SessionFailed),
            154 => Some(EventType::UsageResponse),
            200 => Some(EventType::TaskRequest),
            201 => Some(EventType::UpdateConfig),
            250 => Some(EventType::AudioMuted),
            300 => Some(EventType::SayHello),
            350 => Some(EventType::TTSSentenceStart),
            351 => Some(EventType::TTSSentenceEnd),
            352 => Some(EventType::TTSResponse),
            364 => Some(EventType::TTSSubtitle),
            359 => Some(EventType::TTSEnded),
            360 => Some(EventType::PodcastRoundStart),
            361 => Some(EventType::PodcastRoundResponse),
            362 => Some(EventType::PodcastRoundEnd),
            450 => Some(EventType::ASRInfo),
            451 => Some(EventType::ASRResponse),
            459 => Some(EventType::ASREnded),
            500 => Some(EventType::ChatTTSText),
            550 => Some(EventType::ChatResponse),
            559 => Some(EventType::ChatEnded),
            650 => Some(EventType::SourceSubtitleStart),
            651 => Some(EventType::SourceSubtitleResponse),
            652 => Some(EventType::SourceSubtitleEnd),
            653 => Some(EventType::TranslationSubtitleStart),
            654 => Some(EventType::TranslationSubtitleResponse),
            655 => Some(EventType::TranslationSubtitleEnd),
            _ => None,
        }
    }

    /// True for connection-level events whose `session_id` is omitted on
    /// the wire (writer side). Mirrors `_write_session_id`'s skip list:
    /// StartConnection, FinishConnection, ConnectionStarted, ConnectionFailed.
    fn omits_session_id_writer(self) -> bool {
        matches!(
            self,
            EventType::StartConnection
                | EventType::FinishConnection
                | EventType::ConnectionStarted
                | EventType::ConnectionFailed
        )
    }

    /// True for connection-level events whose `session_id` is omitted on
    /// the wire (reader side). Mirrors `_read_session_id`'s skip list —
    /// note `ConnectionFinished` is included here but not on the writer.
    fn omits_session_id_reader(self) -> bool {
        matches!(
            self,
            EventType::StartConnection
                | EventType::FinishConnection
                | EventType::ConnectionStarted
                | EventType::ConnectionFailed
                | EventType::ConnectionFinished
        )
    }

    /// True for events that carry a `connect_id` on the wire (reader side).
    /// Mirrors `_read_connect_id`: ConnectionStarted, ConnectionFailed,
    /// ConnectionFinished.
    fn carries_connect_id(self) -> bool {
        matches!(
            self,
            EventType::ConnectionStarted
                | EventType::ConnectionFailed
                | EventType::ConnectionFinished
        )
    }
}

// ── Message ────────────────────────────────────────────────────────────

/// A Volcano bidirectional-TTS binary message.
///
/// Field defaults mirror the Python `@dataclass` defaults:
/// `version=Version1`, `header_size=HeaderSize4`,
/// `serialization=JSON`, `compression=None`, `type=Invalid`, `flag=NoSeq`,
/// `event=None`, and empty strings / zero integers / empty payload.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Message {
    pub version: VersionBits,
    pub header_size: HeaderSizeBits,
    pub msg_type: MsgType,
    pub flag: MsgTypeFlagBits,
    pub serialization: SerializationBits,
    pub compression: CompressionBits,

    pub event: EventType,
    pub session_id: String,
    pub connect_id: String,
    pub sequence: i32,
    pub error_code: u32,

    pub payload: Vec<u8>,
}

/// Codec error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodecError {
    /// Buffer ran out of bytes while reading.
    UnexpectedEof,
    /// Header too short (need at least 3 bytes to read version/flag/ser).
    HeaderTooShort,
    /// Unknown message type nibble.
    UnknownMsgType(u8),
    /// Unknown flag nibble.
    UnknownFlag(u8),
    /// Unknown version nibble.
    UnknownVersion(u8),
    /// Unknown header-size nibble.
    UnknownHeaderSize(u8),
    /// Unknown serialization nibble.
    UnknownSerialization(u8),
    /// Unknown compression nibble.
    UnknownCompression(u8),
    /// `msg_type` is not one of the supported wire types.
    UnsupportedMsgType(u8),
    /// Trailing bytes after a fully-parsed message.
    UnexpectedTrailing(usize),
    /// A length prefix exceeded `u32::MAX` (or available buffer).
    LengthTooLarge,
}

impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodecError::UnexpectedEof => write!(f, "unexpected end of buffer"),
            CodecError::HeaderTooShort => write!(f, "header too short (need >= 3 bytes)"),
            CodecError::UnknownMsgType(v) => write!(f, "unknown msg type: {v}"),
            CodecError::UnknownFlag(v) => write!(f, "unknown flag: {v}"),
            CodecError::UnknownVersion(v) => write!(f, "unknown version: {v}"),
            CodecError::UnknownHeaderSize(v) => write!(f, "unknown header size: {v}"),
            CodecError::UnknownSerialization(v) => write!(f, "unknown serialization: {v}"),
            CodecError::UnknownCompression(v) => write!(f, "unknown compression: {v}"),
            CodecError::UnsupportedMsgType(v) => write!(f, "unsupported message type: {v}"),
            CodecError::UnexpectedTrailing(n) => {
                write!(f, "unexpected {n} trailing byte(s) after message")
            }
            CodecError::LengthTooLarge => write!(f, "length prefix exceeds u32::MAX"),
        }
    }
}

impl std::error::Error for CodecError {}

/// Result alias for codec operations.
pub type CodecResult<T> = Result<T, CodecError>;

// ── A tiny big-endian cursor over a byte slice ─────────────────────────

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Cursor { buf, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    fn read(&mut self, n: usize) -> CodecResult<&'a [u8]> {
        if self.remaining() < n {
            return Err(CodecError::UnexpectedEof);
        }
        let out = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    fn read_i32_be(&mut self) -> CodecResult<i32> {
        let b = self.read(4)?;
        Ok(i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn read_u32_be(&mut self) -> CodecResult<u32> {
        let b = self.read(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn read_u8(&mut self) -> CodecResult<u8> {
        let b = self.read(1)?;
        Ok(b[0])
    }

    fn read_len_prefixed(&mut self) -> CodecResult<Vec<u8>> {
        let size = self.read_u32_be()? as usize;
        if self.remaining() < size {
            return Err(CodecError::UnexpectedEof);
        }
        let out = self.read(size)?.to_vec();
        Ok(out)
    }

    fn read_len_prefixed_string(&mut self) -> CodecResult<String> {
        let bytes = self.read_len_prefixed()?;
        String::from_utf8(bytes).map_err(|_| CodecError::UnexpectedEof)
    }
}

// ── Message impl ───────────────────────────────────────────────────────

impl Message {
    /// Construct a default-flags `FullClientRequest` `WithEvent` message —
    /// the common shape for all client-side control messages.
    fn full_client_with_event(event: EventType) -> Self {
        Message {
            msg_type: MsgType::FullClientRequest,
            flag: MsgTypeFlagBits::WithEvent,
            event,
            payload: b"{}".to_vec(),
            ..Message::default()
        }
    }

    /// `start_connection` helper (mirrors Python `start_connection`).
    pub fn start_connection() -> Self {
        Self::full_client_with_event(EventType::StartConnection)
    }

    /// `finish_connection` helper (mirrors Python `finish_connection`).
    pub fn finish_connection() -> Self {
        Self::full_client_with_event(EventType::FinishConnection)
    }

    /// `start_session` helper — carries `payload` and `session_id`.
    pub fn start_session(payload: Vec<u8>, session_id: impl Into<String>) -> Self {
        Message {
            msg_type: MsgType::FullClientRequest,
            flag: MsgTypeFlagBits::WithEvent,
            event: EventType::StartSession,
            session_id: session_id.into(),
            payload,
            ..Message::default()
        }
    }

    /// `finish_session` helper — empty `{}` payload, carries `session_id`.
    pub fn finish_session(session_id: impl Into<String>) -> Self {
        Message {
            msg_type: MsgType::FullClientRequest,
            flag: MsgTypeFlagBits::WithEvent,
            event: EventType::FinishSession,
            session_id: session_id.into(),
            payload: b"{}".to_vec(),
            ..Message::default()
        }
    }

    /// `cancel_session` helper — empty `{}` payload, carries `session_id`.
    pub fn cancel_session(session_id: impl Into<String>) -> Self {
        Message {
            msg_type: MsgType::FullClientRequest,
            flag: MsgTypeFlagBits::WithEvent,
            event: EventType::CancelSession,
            session_id: session_id.into(),
            payload: b"{}".to_vec(),
            ..Message::default()
        }
    }

    /// `task_request` helper — carries `payload` and `session_id`.
    pub fn task_request(payload: Vec<u8>, session_id: impl Into<String>) -> Self {
        Message {
            msg_type: MsgType::FullClientRequest,
            flag: MsgTypeFlagBits::WithEvent,
            event: EventType::TaskRequest,
            session_id: session_id.into(),
            payload,
            ..Message::default()
        }
    }

    // ── marshal ────────────────────────────────────────────────────────

    /// Serialize this message to bytes.
    ///
    /// Port of Python `Message.marshal`. The writer order is:
    ///  1. 4-byte header (version|hdrsize, type|flag, ser|comp, 0 pad)
    ///  2. if `flag == WithEvent`: event (i32 BE), session_id (u32 BE len +
    ///     utf8) — session_id skipped for connection-level events
    ///     (StartConnection / FinishConnection / ConnectionStarted /
    ///     ConnectionFailed)
    ///  3. if `msg_type` in the seq-capable set and `flag` in
    ///     [PositiveSeq, NegativeSeq]: sequence (i32 BE)
    ///     elif `msg_type == Error`: error_code (u32 BE)
    ///     else (and not the seq path): unsupported -> error
    ///  4. payload (u32 BE len + bytes)
    pub fn marshal(&self) -> CodecResult<Vec<u8>> {
        let mut out = Vec::new();

        // --- Header (3 meaningful bytes + padding to 4*header_size) ---
        out.push((self.version.as_u8() << 4) | self.header_size.as_u8());
        out.push((self.msg_type.as_u8() << 4) | self.flag.as_u8());
        out.push((self.serialization.as_u8() << 4) | self.compression.as_u8());

        let header_size_bytes = 4 * self.header_size.as_u8() as usize;
        let padding = header_size_bytes.saturating_sub(out.len());
        out.extend(std::iter::repeat_n(0u8, padding));

        // --- Writers (verbatim order from _get_writers) ---

        // if flag == WithEvent: write event, then session_id
        if self.flag == MsgTypeFlagBits::WithEvent {
            self.write_event(&mut out)?;
            self.write_session_id(&mut out)?;
        }

        // if type in [FullClientRequest, FullServerResponse, FrontEndResultServer,
        //             AudioOnlyClient, AudioOnlyServer]:
        //     if flag in [PositiveSeq, NegativeSeq]: write sequence
        // elif type == Error: write error_code
        // else: unsupported
        let seq_capable = matches!(
            self.msg_type,
            MsgType::FullClientRequest
                | MsgType::FullServerResponse
                | MsgType::FrontEndResultServer
                | MsgType::AudioOnlyClient
                | MsgType::AudioOnlyServer
        );
        if seq_capable {
            if matches!(
                self.flag,
                MsgTypeFlagBits::PositiveSeq | MsgTypeFlagBits::NegativeSeq
            ) {
                self.write_sequence(&mut out);
            }
        } else if self.msg_type == MsgType::Error {
            self.write_error_code(&mut out);
        } else {
            return Err(CodecError::UnsupportedMsgType(self.msg_type.as_u8()));
        }

        // finally: payload
        self.write_payload(&mut out)?;

        Ok(out)
    }

    fn write_event(&self, out: &mut Vec<u8>) -> CodecResult<()> {
        out.extend_from_slice(&self.event.as_i32().to_be_bytes());
        Ok(())
    }

    fn write_session_id(&self, out: &mut Vec<u8>) -> CodecResult<()> {
        // Skip for connection-level events (writer side list).
        if self.event.omits_session_id_writer() {
            return Ok(());
        }
        let bytes = self.session_id.as_bytes();
        let size = u32::try_from(bytes.len()).map_err(|_| CodecError::LengthTooLarge)?;
        out.extend_from_slice(&size.to_be_bytes());
        out.extend_from_slice(bytes);
        Ok(())
    }

    fn write_sequence(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.sequence.to_be_bytes());
    }

    fn write_error_code(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.error_code.to_be_bytes());
    }

    fn write_payload(&self, out: &mut Vec<u8>) -> CodecResult<()> {
        let size = u32::try_from(self.payload.len()).map_err(|_| CodecError::LengthTooLarge)?;
        out.extend_from_slice(&size.to_be_bytes());
        out.extend_from_slice(&self.payload);
        Ok(())
    }

    // ── unmarshal ──────────────────────────────────────────────────────

    /// Parse a message from bytes.
    ///
    /// Port of Python `Message.from_bytes` + `unmarshal`. Reads byte 1 for
    /// `msg_type`/`flag` first (to select readers), then re-parses the full
    /// header via a cursor.
    pub fn unmarshal(data: &[u8]) -> CodecResult<Self> {
        if data.len() < 3 {
            return Err(CodecError::HeaderTooShort);
        }

        // Peek byte 1 to determine msg_type / flag (drives reader selection).
        let type_and_flag = data[1];
        let msg_type = MsgType::from_u8(type_and_flag >> 4)
            .ok_or(CodecError::UnknownMsgType(type_and_flag >> 4))?;
        let flag = MsgTypeFlagBits::from_u8(type_and_flag & 0x0F)
            .ok_or(CodecError::UnknownFlag(type_and_flag & 0x0F))?;

        let mut msg = Message {
            msg_type,
            flag,
            ..Message::default()
        };

        let mut cur = Cursor::new(data);

        // byte 0: version | header_size
        let b0 = cur.read_u8()?;
        let version_nibble = b0 >> 4;
        let header_nibble = b0 & 0x0F;
        msg.version = VersionBits::from_u8_safe(version_nibble)?;
        msg.header_size = HeaderSizeBits::from_u8_safe(header_nibble)?;

        // byte 1: type | flag (already peeked; consume to advance cursor)
        let _ = cur.read_u8()?;

        // byte 2: serialization | compression
        let b2 = cur.read_u8()?;
        msg.serialization = SerializationBits::from_u8_safe(b2 >> 4)?;
        msg.compression = CompressionBits::from_u8_safe(b2 & 0x0F)?;

        // Skip header padding (4*header_size total, we've read 3).
        let header_size_bytes = 4 * msg.header_size.as_u8() as usize;
        let read_so_far = 3usize;
        if header_size_bytes > read_so_far {
            cur.read(header_size_bytes - read_so_far)?;
        }

        // --- Readers (verbatim order from _get_readers) ---

        // if type in [...seq-capable...] and flag in [PositiveSeq, NegativeSeq]:
        //     read sequence
        // elif type == Error: read error_code
        // else: unsupported
        let seq_capable = matches!(
            msg.msg_type,
            MsgType::FullClientRequest
                | MsgType::FullServerResponse
                | MsgType::FrontEndResultServer
                | MsgType::AudioOnlyClient
                | MsgType::AudioOnlyServer
        );
        if seq_capable {
            if matches!(
                msg.flag,
                MsgTypeFlagBits::PositiveSeq | MsgTypeFlagBits::NegativeSeq
            ) {
                msg.sequence = cur.read_i32_be()?;
            }
        } else if msg.msg_type == MsgType::Error {
            msg.error_code = cur.read_u32_be()?;
        } else {
            return Err(CodecError::UnsupportedMsgType(msg.msg_type.as_u8()));
        }

        // if flag == WithEvent: read event, session_id, connect_id
        if msg.flag == MsgTypeFlagBits::WithEvent {
            msg.read_event(&mut cur)?;
            msg.read_session_id(&mut cur)?;
            msg.read_connect_id(&mut cur)?;
        }

        // finally: payload
        msg.payload = cur.read_len_prefixed()?;

        // Reject trailing bytes (mirrors Python's `remaining` check).
        if cur.remaining() != 0 {
            return Err(CodecError::UnexpectedTrailing(cur.remaining()));
        }

        Ok(msg)
    }

    fn read_event(&mut self, cur: &mut Cursor<'_>) -> CodecResult<()> {
        let value = cur.read_i32_be()?;
        // Python tolerates unknown event values by storing the raw int. We
        // only hold known variants; map unknown to `None` so the field is
        // still set to a valid discriminant (the raw value is lost, matching
        // the "best effort" behavior for undefined events).
        self.event = EventType::from_i32(value).unwrap_or(EventType::None);
        Ok(())
    }

    fn read_session_id(&mut self, cur: &mut Cursor<'_>) -> CodecResult<()> {
        // Skip for connection-level events (reader side list — includes
        // ConnectionFinished, which the writer does NOT skip).
        if self.event.omits_session_id_reader() {
            return Ok(());
        }
        self.session_id = cur.read_len_prefixed_string()?;
        Ok(())
    }

    fn read_connect_id(&mut self, cur: &mut Cursor<'_>) -> CodecResult<()> {
        // Only for ConnectionStarted / ConnectionFailed / ConnectionFinished.
        if !self.event.carries_connect_id() {
            return Ok(());
        }
        self.connect_id = cur.read_len_prefixed_string()?;
        Ok(())
    }
}

// ── nibble-safe constructors for the bit enums ─────────────────────────

impl VersionBits {
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    fn from_u8_safe(value: u8) -> CodecResult<Self> {
        match value {
            1 => Ok(VersionBits::Version1),
            2 => Ok(VersionBits::Version2),
            3 => Ok(VersionBits::Version3),
            4 => Ok(VersionBits::Version4),
            _ => Err(CodecError::UnknownVersion(value)),
        }
    }
}

impl HeaderSizeBits {
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    fn from_u8_safe(value: u8) -> CodecResult<Self> {
        match value {
            1 => Ok(HeaderSizeBits::HeaderSize4),
            2 => Ok(HeaderSizeBits::HeaderSize8),
            3 => Ok(HeaderSizeBits::HeaderSize12),
            4 => Ok(HeaderSizeBits::HeaderSize16),
            _ => Err(CodecError::UnknownHeaderSize(value)),
        }
    }
}

impl SerializationBits {
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    fn from_u8_safe(value: u8) -> CodecResult<Self> {
        match value {
            0 => Ok(SerializationBits::Raw),
            0b1 => Ok(SerializationBits::JSON),
            0b11 => Ok(SerializationBits::Thrift),
            0b1111 => Ok(SerializationBits::Custom),
            _ => Err(CodecError::UnknownSerialization(value)),
        }
    }
}

impl CompressionBits {
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    fn from_u8_safe(value: u8) -> CodecResult<Self> {
        match value {
            0 => Ok(CompressionBits::None),
            0b1 => Ok(CompressionBits::Gzip),
            0b1111 => Ok(CompressionBits::Custom),
            _ => Err(CodecError::UnknownCompression(value)),
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: assert a marshalled message begins with the standard 4-byte
    /// header `[0x11, type|flag, 0x10, 0x00]` (version=1, header_size=1,
    /// JSON serialization, no compression).
    fn assert_standard_header(bytes: &[u8], msg_type: MsgType, flag: MsgTypeFlagBits) {
        assert_eq!(bytes[0], 0x11, "version|header_size byte must be 0x11");
        assert_eq!(
            bytes[1],
            (msg_type.as_u8() << 4) | flag.as_u8(),
            "type|flag byte mismatch"
        );
        assert_eq!(
            bytes[2], 0x10,
            "serialization|compression must be 0x10 (JSON|None)"
        );
        assert_eq!(bytes[3], 0x00, "header padding byte must be 0x00");
    }

    #[test]
    fn start_connection_round_trip() {
        let msg = Message::start_connection();
        let bytes = msg.marshal().expect("marshal start_connection");

        // Header: version=1, header_size=1, FullClientRequest|WithEvent, JSON|None, pad.
        assert_standard_header(
            &bytes,
            MsgType::FullClientRequest,
            MsgTypeFlagBits::WithEvent,
        );

        // Body: event (i32 BE = 1), no session_id (StartConnection skips it),
        // payload = b"{}" (u32 BE len=2 + bytes).
        let mut expected = vec![
            0x11,
            (MsgType::FullClientRequest.as_u8() << 4) | MsgTypeFlagBits::WithEvent.as_u8(),
            0x10,
            0x00,
        ];
        expected.extend_from_slice(&1i32.to_be_bytes()); // event = StartConnection
        expected.extend_from_slice(&2u32.to_be_bytes()); // payload len
        expected.extend_from_slice(b"{}");
        assert_eq!(bytes, expected);

        // Round-trip.
        let back = Message::unmarshal(&bytes).expect("unmarshal start_connection");
        assert_eq!(back, msg);
    }

    #[test]
    fn finish_connection_round_trip() {
        let msg = Message::finish_connection();
        let bytes = msg.marshal().expect("marshal finish_connection");

        assert_standard_header(
            &bytes,
            MsgType::FullClientRequest,
            MsgTypeFlagBits::WithEvent,
        );

        // event = FinishConnection (2); no session_id; payload "{}".
        let mut expected = vec![0x11, 0x14, 0x10, 0x00];
        expected.extend_from_slice(&2i32.to_be_bytes());
        expected.extend_from_slice(&2u32.to_be_bytes());
        expected.extend_from_slice(b"{}");
        assert_eq!(bytes, expected);

        let back = Message::unmarshal(&bytes).expect("unmarshal finish_connection");
        assert_eq!(back, msg);
    }

    #[test]
    fn start_session_round_trip() {
        let payload = br#"{"user":{"uid":"42"}}"#.to_vec();
        let session_id = "sess-1234".to_string();
        let msg = Message::start_session(payload.clone(), session_id.clone());
        let bytes = msg.marshal().expect("marshal start_session");

        assert_standard_header(
            &bytes,
            MsgType::FullClientRequest,
            MsgTypeFlagBits::WithEvent,
        );

        // event = StartSession (100), session_id (len + utf8), payload (len + bytes).
        let mut expected = vec![0x11, 0x14, 0x10, 0x00];
        expected.extend_from_slice(&100i32.to_be_bytes()); // event
        expected.extend_from_slice(&(session_id.len() as u32).to_be_bytes());
        expected.extend_from_slice(session_id.as_bytes());
        expected.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        expected.extend_from_slice(&payload);
        assert_eq!(bytes, expected);

        let back = Message::unmarshal(&bytes).expect("unmarshal start_session");
        assert_eq!(back, msg);
        assert_eq!(back.event, EventType::StartSession);
        assert_eq!(back.session_id, session_id);
        assert_eq!(back.payload, payload);
    }

    #[test]
    fn task_request_round_trip() {
        let payload: Vec<u8> = br#"{"text":"hello","speaker":"zh-Male"}"#.to_vec();
        let session_id = "task-sess-9999".to_string();
        let msg = Message::task_request(payload.clone(), session_id.clone());
        let bytes = msg.marshal().expect("marshal task_request");

        assert_standard_header(
            &bytes,
            MsgType::FullClientRequest,
            MsgTypeFlagBits::WithEvent,
        );

        let mut expected = vec![0x11, 0x14, 0x10, 0x00];
        expected.extend_from_slice(&200i32.to_be_bytes()); // TaskRequest
        expected.extend_from_slice(&(session_id.len() as u32).to_be_bytes());
        expected.extend_from_slice(session_id.as_bytes());
        expected.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        expected.extend_from_slice(&payload);
        assert_eq!(bytes, expected);

        let back = Message::unmarshal(&bytes).expect("unmarshal task_request");
        assert_eq!(back, msg);
        assert_eq!(back.event, EventType::TaskRequest);
    }

    #[test]
    fn finish_session_round_trip() {
        let session_id = "sess-finish-1".to_string();
        let msg = Message::finish_session(session_id.clone());
        let bytes = msg.marshal().expect("marshal finish_session");

        assert_standard_header(
            &bytes,
            MsgType::FullClientRequest,
            MsgTypeFlagBits::WithEvent,
        );

        let mut expected = vec![0x11, 0x14, 0x10, 0x00];
        expected.extend_from_slice(&102i32.to_be_bytes()); // FinishSession
        expected.extend_from_slice(&(session_id.len() as u32).to_be_bytes());
        expected.extend_from_slice(session_id.as_bytes());
        expected.extend_from_slice(&2u32.to_be_bytes());
        expected.extend_from_slice(b"{}");
        assert_eq!(bytes, expected);

        let back = Message::unmarshal(&bytes).expect("unmarshal finish_session");
        assert_eq!(back, msg);
        assert_eq!(back.event, EventType::FinishSession);
        assert_eq!(back.session_id, session_id);
    }

    #[test]
    fn cancel_session_round_trip() {
        let session_id = "sess-cancel-7".to_string();
        let msg = Message::cancel_session(session_id.clone());
        let bytes = msg.marshal().expect("marshal cancel_session");
        assert_standard_header(
            &bytes,
            MsgType::FullClientRequest,
            MsgTypeFlagBits::WithEvent,
        );
        let back = Message::unmarshal(&bytes).expect("unmarshal cancel_session");
        assert_eq!(back, msg);
        assert_eq!(back.event, EventType::CancelSession);
    }

    #[test]
    fn header_byte_zero_is_0x11() {
        // All real client messages start with 0x11 (version=1, header_size=1).
        for bytes in [
            Message::start_connection().marshal(),
            Message::finish_connection().marshal(),
            Message::start_session(vec![], "s").marshal(),
            Message::task_request(vec![], "s").marshal(),
            Message::finish_session("s").marshal(),
            Message::cancel_session("s").marshal(),
        ] {
            let b = bytes.expect("marshal ok");
            assert_eq!(b[0], 0x11, "first byte must be 0x11");
        }
    }

    #[test]
    fn empty_payload_and_empty_session_id_round_trip() {
        // session_id "" -> writer still emits a u32 len=0 prefix (only
        // connection-level events skip the prefix entirely).
        let msg = Message::start_session(vec![], "");
        let bytes = msg.marshal().expect("marshal");
        let back = Message::unmarshal(&bytes).expect("unmarshal");
        assert_eq!(back, msg);
        assert_eq!(back.session_id, "");
        assert!(back.payload.is_empty());
    }

    #[test]
    fn rejects_trailing_bytes() {
        let mut bytes = Message::start_connection().marshal().expect("marshal");
        bytes.push(0xFF); // one extra byte
        let err = Message::unmarshal(&bytes).unwrap_err();
        assert_eq!(err, CodecError::UnexpectedTrailing(1));
    }

    #[test]
    fn rejects_header_too_short() {
        let err = Message::unmarshal(&[0x11, 0x14]).unwrap_err();
        assert_eq!(err, CodecError::HeaderTooShort);
    }

    #[test]
    fn rejects_unsupported_msg_type() {
        // type nibble = 0 (Invalid) is not a supported wire type.
        // byte1 = (0<<4)|NoSeq = 0x00
        let bytes = [0x11, 0x00, 0x10, 0x00];
        let err = Message::unmarshal(&bytes).unwrap_err();
        assert_eq!(err, CodecError::UnsupportedMsgType(0));
    }

    #[test]
    fn error_message_round_trip() {
        // Error type carries an error_code (u32 BE) instead of a sequence.
        let msg = Message {
            msg_type: MsgType::Error,
            flag: MsgTypeFlagBits::NoSeq,
            error_code: 0x4001,
            payload: b"boom".to_vec(),
            ..Message::default()
        };
        let bytes = msg.marshal().expect("marshal error");

        // Header byte1 = (Error<<4)|NoSeq = 0xF0.
        assert_standard_header(&bytes, MsgType::Error, MsgTypeFlagBits::NoSeq);

        // Body: error_code (u32 BE), then payload (len + bytes). No event,
        // no session_id (flag != WithEvent).
        let mut expected = vec![0x11, 0xF0, 0x10, 0x00];
        expected.extend_from_slice(&0x4001u32.to_be_bytes());
        expected.extend_from_slice(&4u32.to_be_bytes());
        expected.extend_from_slice(b"boom");
        assert_eq!(bytes, expected);

        let back = Message::unmarshal(&bytes).expect("unmarshal error");
        assert_eq!(back, msg);
        assert_eq!(back.error_code, 0x4001);
    }

    #[test]
    fn audio_only_server_positive_seq_round_trip() {
        // AudioOnlyServer + PositiveSeq carries a sequence (i32 BE) before
        // the payload. This is the real audio-frame shape (no event).
        let msg = Message {
            msg_type: MsgType::AudioOnlyServer,
            flag: MsgTypeFlagBits::PositiveSeq,
            sequence: 42,
            payload: vec![0x01, 0x02, 0x03],
            ..Message::default()
        };
        let bytes = msg.marshal().expect("marshal audio");

        // Header byte1 = (AudioOnlyServer<<4)|PositiveSeq = 0xB1.
        assert_standard_header(
            &bytes,
            MsgType::AudioOnlyServer,
            MsgTypeFlagBits::PositiveSeq,
        );

        let mut expected = vec![0x11, 0xB1, 0x10, 0x00];
        expected.extend_from_slice(&42i32.to_be_bytes()); // sequence
        expected.extend_from_slice(&3u32.to_be_bytes()); // payload len
        expected.extend_from_slice(&[0x01, 0x02, 0x03]);
        assert_eq!(bytes, expected);

        let back = Message::unmarshal(&bytes).expect("unmarshal audio");
        assert_eq!(back, msg);
        assert_eq!(back.sequence, 42);
    }

    #[test]
    fn connect_id_skips_session_id_but_reads_connect_id() {
        // ConnectionStarted is downstream: reader skips session_id AND reads
        // connect_id. Simulate a server-style message: FullServerResponse is
        // NOT used with WithEvent in practice, so use FullClientRequest shape
        // but set event=ConnectionStarted to exercise the reader branches.
        let msg = Message {
            msg_type: MsgType::FullClientRequest,
            flag: MsgTypeFlagBits::WithEvent,
            event: EventType::ConnectionStarted,
            connect_id: "conn-abc".to_string(),
            payload: b"{}".to_vec(),
            ..Message::default()
        };
        let bytes = msg.marshal().expect("marshal connection_started");

        // Writer: WithEvent -> event; session_id is skipped (ConnectionStarted
        // is in the writer skip list), so no session_id prefix; then payload.
        let mut expected = vec![0x11, 0x14, 0x10, 0x00];
        expected.extend_from_slice(&50i32.to_be_bytes()); // ConnectionStarted
        // NB: writer does NOT emit connect_id at all (Python has no
        // _write_connect_id). connect_id is read-only on the server side.
        expected.extend_from_slice(&2u32.to_be_bytes()); // payload len
        expected.extend_from_slice(b"{}");
        assert_eq!(bytes, expected);

        // Reader: WithEvent -> event; session_id skipped (ConnectionStarted
        // in reader skip list); connect_id READ (ConnectionStarted carries it).
        let back = Message::unmarshal(&bytes).expect("unmarshal connection_started");
        assert_eq!(back.event, EventType::ConnectionStarted);
        assert_eq!(back.connect_id, ""); // writer never wrote it -> empty
        assert_eq!(back.payload, b"{}");
    }

    // ── Cross-validation against the Python `protocols_.py` reference ──────
    //
    // Authoritative marshal bytes were produced by running the Python
    // `Message.marshal()` on byte-for-byte identical message objects (see
    // verification notes). The JSON payload for `start_session` is the exact
    // output of Python's `json.dumps(...)` (default separators -> `, ` / `: `),
    // passed verbatim to the Rust helper so both sides serialize the same
    // payload bytes.
    fn hex_to_bytes(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn python_reference_start_connection() {
        let bytes = Message::start_connection().marshal().expect("marshal");
        let py = hex_to_bytes("1114100000000001000000027b7d");
        assert_eq!(bytes, py, "start_connection mismatch");
    }

    #[test]
    fn python_reference_start_session() {
        // Payload is the exact output of Python json.dumps with default
        // separators — passed as raw bytes to the Rust helper.
        let payload: Vec<u8> = br#"{"req_params": {"speaker": "S", "audio_params": {"format": "pcm", "sample_rate": 16000}}}"#.to_vec();
        let bytes = Message::start_session(payload, "sess-123")
            .marshal()
            .expect("marshal");
        let py = hex_to_bytes(
            "111410000000006400000008736573732d313233000000597b227265715f706172\
             616d73223a207b22737065616b6572223a202253222c2022617564696f5f706172\
             616d73223a207b22666f726d6174223a202270636d222c202273616d706c655f72\
             617465223a2031363030307d7d7d",
        );
        assert_eq!(bytes, py, "start_session mismatch");
    }

    #[test]
    fn python_reference_task_request() {
        let bytes = Message::task_request(b"hello".to_vec(), "sess-123")
            .marshal()
            .expect("marshal");
        let py = hex_to_bytes("11141000000000c800000008736573732d3132330000000568656c6c6f");
        assert_eq!(bytes, py, "task_request mismatch");
    }

    #[test]
    fn python_reference_finish_session() {
        let bytes = Message::finish_session("sess-123")
            .marshal()
            .expect("marshal");
        let py = hex_to_bytes("111410000000006600000008736573732d313233000000027b7d");
        assert_eq!(bytes, py, "finish_session mismatch");
    }

    #[test]
    fn python_reference_finish_connection() {
        let bytes = Message::finish_connection().marshal().expect("marshal");
        let py = hex_to_bytes("1114100000000002000000027b7d");
        assert_eq!(bytes, py, "finish_connection mismatch");
    }
}
