/// Pipeline native sample rate: 8 kHz. Caller (sofia) audio is 8 kHz, TTS
/// (seed-tts-2.0) outputs 8 kHz, LLM accepts 8 kHz WAV — so the entire
/// pipeline (speech_buffer, ringbuf, codec, TTS, LLM WAV) runs at 8 kHz
/// to avoid unnecessary resampling. Only the VAD (earshot) requires 16 kHz,
/// handled as a bypass upsampling path.
pub const PIPELINE_SAMPLE_RATE: u32 = 8000;

/// VAD sample rate: earshot requires 16 kHz input. Caller 8 kHz audio is
/// upsampled to 16 kHz for VAD prediction only; the speech segment data
/// stays at 8 kHz (pipeline native).
pub const VAD_SAMPLE_RATE: u32 = 16000;

/// Audio-output callback: the TTS driver invokes this for each resampled PCM
/// chunk, pushing directly into the caller's playback ringbuf (no mpsc/forwarder).
pub type OnAudio = Box<dyn FnMut(&[i16]) + Send + 'static>;

/// `fswtch::Resample` (`switch_resample_t`) is `!Send`/`!Sync` because the
/// underlying C resampler is not safe under *concurrent* access. Both call
/// sites (the media-thread VAD bypass in `io.rs` and the tokio TTS driver
/// task in `tts.rs`) hold one resampler and run it single-threaded — task
/// migration / thread ownership provides a happens-before relationship, so
/// exclusive ownership across `.await` points is sound. We opt into
/// `Send + Sync` here, in ONE place, to satisfy both call sites and keep the
/// `unsafe` justification centralized.
pub(crate) struct SendResample(pub(crate) fswtch::Resample);
// SAFETY: the wrapped resampler is only ever touched from the single task/thread
// that owns it; it is never shared across threads concurrently.
unsafe impl Send for SendResample {}
unsafe impl Sync for SendResample {}

/// Get the codec sample rate from a FreeSWITCH session.
///
/// Returns the session's read-codec sample rate (Hz), defaulting to 8000
/// when no codec is set. Thin wrapper over `fswtch::Session::read_sample_rate`.
pub fn get_codec_rate(session: &fswtch::Session) -> u32 {
    session.read_sample_rate()
}
