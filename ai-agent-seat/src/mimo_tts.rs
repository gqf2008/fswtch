//! Xiaomi MIMO HTTP TTS client.
//!
//! Unlike Volcano (WebSocket bidirectional, call-lifetime session), MIMO TTS
//! uses the **chat/completions** endpoint: one HTTP POST per sentence that
//! returns a base64-encoded WAV (or PCM16) in the JSON response body.
//! There is no persistent connection — each [`synthesize`] call is a
//! standalone HTTP request.
//!
//! # Audio path
//!
//! The response carries WAV data (typically 24 kHz mono 16-bit). We parse
//! the WAV header, resample to [`PIPELINE_SAMPLE_RATE`] (8 kHz), and push
//! the PCM through the call-wide `on_audio` callback (→ SPSC ringbuf →
//! `io::read_frame`). One callback for the whole call.
//!
//! # Cancellation
//!
//! MIMO TTS is a single blocking HTTP call — there is no streaming audio to
//! cancel mid-stream. We set a cancelled flag that causes the callback to
//! be skipped if barge-in arrived before the response completed.

use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use base64::Engine;
use tokio_util::sync::CancellationToken;

use crate::audio_dsp::{OnAudio, PIPELINE_SAMPLE_RATE, SendResample};
use crate::tts::OnTurnEnd;

/// A MIMO TTS handle bound to one call.
///
/// Constructed cheaply (sync); each [`synthesize`](Self::synthesize) call
/// issues a standalone HTTP POST to the MIMO chat/completions endpoint.
/// Cloning is cheap (Arc inner).
#[derive(Clone)]
pub struct MimoTtsHandle {
    inner: Arc<MimoTtsInner>,
}

struct MimoTtsInner {
    api_key: String,
    base_url: String,
    voice: String,
    format: String,
    call_uuid: String,
    on_audio: parking_lot::Mutex<OnAudio>,
    on_turn_end: parking_lot::Mutex<OnTurnEnd>,
    cancelled: AtomicBool,
    http_client: reqwest::Client,
}

impl MimoTtsHandle {
    /// Build a handle for the given credentials + voice, bound to the call UUID.
    ///
    /// `on_audio` is invoked with each chunk of resampled PCM (8 kHz i16).
    /// `on_turn_end` is invoked once synthesis completes.
    pub fn new(
        api_key: String,
        base_url: String,
        voice: String,
        format: String,
        call_uuid: String,
        on_audio: OnAudio,
        on_turn_end: OnTurnEnd,
    ) -> Self {
        let voice = if voice.is_empty() {
            "mimo_default".to_string()
        } else {
            voice
        };
        let format = if format.is_empty() {
            "wav".to_string()
        } else {
            format
        };
        tracing::debug!(
            "MIMO TTS handle created: call_uuid={} voice={} format={}",
            call_uuid,
            voice,
            format
        );
        Self {
            inner: Arc::new(MimoTtsInner {
                api_key,
                base_url,
                voice,
                format,
                call_uuid,
                on_audio: parking_lot::Mutex::new(on_audio),
                on_turn_end: parking_lot::Mutex::new(on_turn_end),
                cancelled: AtomicBool::new(false),
                http_client: reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(30))
                    .build()
                    .unwrap_or_else(|_| reqwest::Client::new()),
            }),
        }
    }

    /// No-op: MIMO has no persistent connection to establish.
    pub async fn start(&self) -> Result<()> {
        Ok(())
    }

    /// Synthesize `text` via MIMO chat/completions + audio output, then push
    /// the PCM through `on_audio`. Returns `Ok(true)` on success.
    ///
    /// Uses **streaming mode** (`stream: true` + `format: pcm16`) when possible:
    /// the server returns SSE chunks with base64-encoded PCM16 (24kHz) that are
    /// decoded + resampled to 8kHz and pushed to the ringbuf **as they arrive**
    /// — first audio latency is ~300-500ms instead of waiting for the full
    /// synthesis (~3s).
    pub async fn synthesize(&self, text: &str, cancel: CancellationToken) -> Result<bool> {
        if cancel.is_cancelled() || self.inner.cancelled.load(Ordering::Relaxed) {
            return Ok(false);
        }

        let t_syn = std::time::Instant::now();
        tracing::info!(
            "MIMO TTS synthesize: {} chars (voice={})",
            text.chars().count(),
            self.inner.voice
        );

        // Build the streaming request: pcm16 format + stream:true.
        // pcm16 returns raw 24kHz PCM16LE chunks via SSE delta.audio.data.
        let body = serde_json::json!({
            "model": "mimo-v2.5-tts",
            "messages": [{"role": "assistant", "content": text}],
            "audio": {"voice": self.inner.voice, "format": "pcm16"},
            "stream": true
        });

        let url = format!(
            "{}/chat/completions",
            self.inner.base_url.trim_end_matches('/')
        );

        let resp = self
            .inner
            .http_client
            .post(&url)
            .header("api-key", &self.inner.api_key)
            .json(&body)
            .send()
            .await
            .context("MIMO TTS HTTP request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("MIMO TTS HTTP {status}: {text}");
        }

        // Stream SSE chunks: decode base64 PCM → resample 24k→8k → push ringbuf.
        use futures::StreamExt;
        let mut stream = resp.bytes_stream();
        let mut buffer = String::new();
        let mut first_chunk_seen = false;
        // One resampler for the whole synthesis (carries state across chunks).
        let mut resampler = SendResample(
            fswtch::Resample::new(24000, PIPELINE_SAMPLE_RATE, 1, 1).map_err(|e| {
                anyhow::anyhow!("MIMO TTS resampler init (24000→{PIPELINE_SAMPLE_RATE}): {e:?}")
            })?,
        );

        loop {
            let chunk = tokio::select! {
                biased;
                _ = cancel.cancelled() => break,
                c = stream.next() => match c {
                    Some(c) => c,
                    None => break,
                },
            };
            let bytes = chunk?;
            buffer.push_str(std::str::from_utf8(&bytes).unwrap_or(""));
            if buffer.contains('\r') {
                buffer.retain(|c| c != '\r');
            }
            while let Some(pos) = buffer.find("\n\n") {
                let event_block: String = buffer.drain(..pos).collect();
                buffer.drain(..2);
                let mut assembled = String::new();
                for raw in event_block.lines() {
                    let line = raw.trim();
                    if line.is_empty() || line.starts_with(':') {
                        continue;
                    }
                    if let Some(d) = line.strip_prefix("data: ") {
                        if !assembled.is_empty() {
                            assembled.push('\n');
                        }
                        assembled.push_str(d);
                    }
                }
                if assembled.is_empty() {
                    continue;
                }
                if assembled == "[DONE]" {
                    buffer.clear();
                    break;
                }
                let parsed: serde_json::Value = match serde_json::from_str(&assembled) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                // Extract delta.audio.data (base64 PCM16 24kHz).
                if let Some(audio_b64) = parsed
                    .get("choices")
                    .and_then(|c| c.get(0))
                    .and_then(|c| c.get("delta"))
                    .and_then(|d| d.get("audio"))
                    .and_then(|a| a.get("data"))
                    .and_then(|d| d.as_str())
                {
                    if !first_chunk_seen {
                        first_chunk_seen = true;
                        tracing::info!(
                            "LATENCY MIMO TTS {}: first PCM chunk = {}ms",
                            self.inner.call_uuid,
                            t_syn.elapsed().as_millis()
                        );
                    }
                    // Decode base64 → raw PCM bytes → i16 samples.
                    let pcm_bytes = base64::engine::general_purpose::STANDARD
                        .decode(audio_b64)
                        .context("MIMO TTS base64 decode failed")?;
                    let samples: Vec<i16> = pcm_bytes
                        .chunks_exact(2)
                        .map(|b| i16::from_le_bytes([b[0], b[1]]))
                        .collect();
                    // Resample 24kHz → 8kHz, push to ringbuf immediately.
                    let mut buf = samples;
                    let out = resampler.0.process(&mut buf).to_vec();
                    if !out.is_empty() {
                        let mut cb = self.inner.on_audio.lock();
                        cb(&out);
                    }
                }
            }
        }

        // Signal turn completion.
        let mut cb = self.inner.on_turn_end.lock();
        cb();

        tracing::info!(
            "LATENCY MIMO TTS {}: total synthesize = {}ms",
            self.inner.call_uuid,
            t_syn.elapsed().as_millis()
        );

        Ok(true)
    }

    /// Mark the handle cancelled (barge-in). The next [`synthesize`] call
    /// will return `Ok(false)` without making an HTTP request. If an HTTP
    /// call is already in flight, its audio will be discarded when the
    /// response arrives.
    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::Relaxed);
    }

    /// Push PCM through `on_audio`.
    fn push_audio(&self, pcm: &[i16]) {
        if !pcm.is_empty() {
            let mut cb = self.inner.on_audio.lock();
            cb(pcm);
        }
    }
}

// ── WAV parser ──────────────────────────────────────────────────────────

/// Parse a WAV byte slice into raw i16 PCM samples + sample rate (Hz).
///
/// Supports standard 44-byte WAV header with format chunk (`fmt `) and data
/// chunk (`data`). Only PCM format (AudioFormat = 1), 16-bit mono or stereo.
/// Stereo is downmixed to mono by averaging channels.
fn decode_wav_to_pcm(bytes: &[u8]) -> Result<(Vec<i16>, u32)> {
    let mut cursor = Cursor::new(bytes);

    // ── RIFF header ────────────────────────────────────────
    let mut riff_id = [0u8; 4];
    read_exact(&mut cursor, &mut riff_id)?;
    if &riff_id != b"RIFF" {
        anyhow::bail!("not a RIFF file");
    }
    // Skip file size (4 bytes) + WAVE id (4 bytes).
    let mut wave_id = [0u8; 4];
    read_le_u32(&mut cursor)?; // file size
    read_exact(&mut cursor, &mut wave_id)?;
    if &wave_id != b"WAVE" {
        anyhow::bail!("not a WAVE file");
    }

    let mut sample_rate: u32 = 0;
    let mut num_channels: u16 = 0;
    let mut bits_per_sample: u16;
    let mut pcm_data: Option<Vec<i16>> = None;

    // ── Chunk loop ─────────────────────────────────────────
    loop {
        let mut chunk_id = [0u8; 4];
        match read_exact(&mut cursor, &mut chunk_id) {
            Ok(_) => {}
            Err(_) => break, // EOF — stop parsing
        }
        let chunk_size = read_le_u32(&mut cursor)?;

        match &chunk_id {
            b"fmt " => {
                let audio_format = read_le_u16(&mut cursor)?;
                if audio_format != 1 {
                    anyhow::bail!("unsupported WAV format: {} (only PCM=1)", audio_format);
                }
                num_channels = read_le_u16(&mut cursor)?;
                sample_rate = read_le_u32(&mut cursor)?;
                let _byte_rate = read_le_u32(&mut cursor)?;
                let _block_align = read_le_u16(&mut cursor)?;
                bits_per_sample = read_le_u16(&mut cursor)?;
                if bits_per_sample != 16 {
                    anyhow::bail!("only 16-bit WAV supported, got {bits_per_sample}-bit");
                }
                // Skip any remaining fmt chunk bytes beyond the standard 16.
                if chunk_size > 16 {
                    let skip = (chunk_size - 16) as usize;
                    let mut discard = vec![0u8; skip.min(4096)];
                    let mut remaining = skip;
                    while remaining > 0 {
                        let n = remaining.min(discard.len());
                        read_exact(&mut cursor, &mut discard[..n])?;
                        remaining -= n;
                    }
                }
            }
            b"data" => {
                let data_size = chunk_size as usize;
                let _num_samples = data_size / 2; // 16-bit
                let mut raw = vec![0u8; data_size];
                read_exact(&mut cursor, &mut raw)?;
                let mut samples: Vec<i16> = raw
                    .chunks_exact(2)
                    .map(|b| i16::from_le_bytes([b[0], b[1]]))
                    .collect();
                // Downmix stereo → mono.
                if num_channels == 2 {
                    let mut mono = Vec::with_capacity(samples.len() / 2);
                    for pair in samples.chunks_exact(2) {
                        // Average of left + right, clamped to i16 range.
                        let avg = (pair[0] as i32 + pair[1] as i32) / 2;
                        mono.push(avg as i16);
                    }
                    samples = mono;
                } else if num_channels > 2 {
                    anyhow::bail!("unsupported channel count: {}", num_channels);
                }
                pcm_data = Some(samples);
                // Continue parsing — some WAV files have metadata after data.
            }
            _ => {
                // Skip unknown chunks.
                let skip = chunk_size as usize;
                let mut remaining = skip;
                // Read in small chunks to avoid allocating a huge vec.
                let mut discard = [0u8; 4096];
                while remaining > 0 {
                    let n = remaining.min(discard.len());
                    read_exact(&mut cursor, &mut discard[..n])?;
                    remaining -= n;
                }
            }
        }
    }

    let pcm = pcm_data.ok_or_else(|| anyhow::anyhow!("WAV file has no data chunk"))?;
    if sample_rate == 0 {
        anyhow::bail!("WAV format chunk missing sample rate");
    }
    Ok((pcm, sample_rate))
}

// ── Helpers ─────────────────────────────────────────────────────────────

fn read_exact(cursor: &mut Cursor<&[u8]>, buf: &mut [u8]) -> Result<()> {
    let pos = cursor.position() as usize;
    let data = cursor.get_ref();
    if pos + buf.len() > data.len() {
        anyhow::bail!("unexpected EOF in WAV at byte {pos}");
    }
    buf.copy_from_slice(&data[pos..pos + buf.len()]);
    cursor.set_position((pos + buf.len()) as u64);
    Ok(())
}

fn read_le_u16(cursor: &mut Cursor<&[u8]>) -> Result<u16> {
    let mut buf = [0u8; 2];
    read_exact(cursor, &mut buf)?;
    Ok(u16::from_le_bytes(buf))
}

fn read_le_u32(cursor: &mut Cursor<&[u8]>) -> Result<u32> {
    let mut buf = [0u8; 4];
    read_exact(cursor, &mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal WAV file (44 bytes + PCM data).
    fn build_wav(pcm: &[i16], sample_rate: u32, channels: u16) -> Vec<u8> {
        let data_size = pcm.len() * 2; // 16-bit
        let file_size = 36 + data_size as u32;
        let mut wav = Vec::with_capacity(44 + data_size);

        // RIFF header
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&file_size.to_le_bytes());
        wav.extend_from_slice(b"WAVE");

        // fmt chunk
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes()); // chunk size
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM format
        wav.extend_from_slice(&channels.to_le_bytes());
        wav.extend_from_slice(&sample_rate.to_le_bytes());
        let byte_rate = sample_rate * channels as u32 * 2; // 16-bit
        wav.extend_from_slice(&byte_rate.to_le_bytes());
        let block_align = channels * 2;
        wav.extend_from_slice(&block_align.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample

        // data chunk
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data_size as u32).to_le_bytes());
        for &s in pcm {
            wav.extend_from_slice(&s.to_le_bytes());
        }

        wav
    }

    #[test]
    fn decode_wav_8khz_mono() {
        let pcm: Vec<i16> = (0..160).map(|i| (i % 1000) as i16).collect();
        let wav = build_wav(&pcm, 8000, 1);
        let (out, rate) = decode_wav_to_pcm(&wav).expect("decode");
        assert_eq!(rate, 8000);
        assert_eq!(out, pcm);
    }

    #[test]
    fn decode_wav_24khz_stereo_downmix() {
        // Stereo: interleaved L,R. 4 samples → 2 mono after downmix.
        let stereo: Vec<i16> = vec![100, 200, 300, 400]; // L,R,L,R
        let wav = build_wav(&stereo, 24000, 2);
        let (out, rate) = decode_wav_to_pcm(&wav).expect("decode");
        assert_eq!(rate, 24000);
        // (100+200)/2=150, (300+400)/2=350
        assert_eq!(out, vec![150, 350]);
    }

    #[test]
    fn decode_wav_unknown_chunk_skipped() {
        // Insert a JUNK chunk between fmt and data — must be skipped.
        let pcm: Vec<i16> = vec![42, 43, 44];
        let data_size = pcm.len() * 2;
        let file_size = 36 + 8 + data_size as u32; // +8 for JUNK chunk header
        let mut wav = Vec::new();

        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&file_size.to_le_bytes());
        wav.extend_from_slice(b"WAVE");

        // fmt chunk
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&8000u32.to_le_bytes());
        wav.extend_from_slice(&16000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());

        // JUNK chunk (4 bytes of junk)
        wav.extend_from_slice(b"JUNK");
        wav.extend_from_slice(&4u32.to_le_bytes());
        wav.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);

        // data chunk
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data_size as u32).to_le_bytes());
        for &s in &pcm {
            wav.extend_from_slice(&s.to_le_bytes());
        }

        let (out, rate) = decode_wav_to_pcm(&wav).expect("decode");
        assert_eq!(rate, 8000);
        assert_eq!(out, pcm);
    }

    #[test]
    fn decode_wav_empty_returns_error() {
        assert!(decode_wav_to_pcm(&[]).is_err());
    }

    #[test]
    fn decode_wav_not_riff() {
        let wav = build_wav(&[1i16, 2], 8000, 1);
        // Corrupt the RIFF id.
        let mut bad = wav;
        bad[0] = b'X';
        assert!(decode_wav_to_pcm(&bad).is_err());
    }
}
