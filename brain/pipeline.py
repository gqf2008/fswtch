"""The "business" side: turn a caller utterance (PCM) into a TTS reply (PCM).

This is where ai-agent-seat's ASR/LLM/TTS orchestration would live — re-implemented in
Python. The default `StubPipeline` just beeps back, which is enough to prove the ESL
plumbing end-to-end (caller speaks → VAD → uplink_pcm → brain → downlink_pcm → caller
hears the beep, with barge-in). Plug a real pipeline (Doubao LLM + Volcano TTS, or
audio-LLM multimodal) by implementing `Pipeline`.
"""

from __future__ import annotations

import math
import struct
from abc import ABC, abstractmethod


class Pipeline(ABC):
    """Turn one caller utterance into a TTS reply.

    `call_id` identifies the call (the B-leg UUID) so stateful pipelines can keep
    per-call conversation history. `pcm_i16` is the raw S16LE (little-endian signed
    16-bit) PCM of the whole utterance, at `sample_rate` Hz / `channels` channels.
    Return raw S16LE PCM at the SAME rate/channels (the fswtch module plays it back
    on the call's write leg). `end_call` is called once when the call hangs up so
    stateful pipelines can drop per-call state.
    """

    @abstractmethod
    def process(self, call_id: str, pcm_i16: bytes, sample_rate: int, channels: int) -> bytes: ...

    def process_stream(self, call_id: str, pcm_i16: bytes, sample_rate: int, channels: int):
        """Streaming version: yield TTS PCM chunks as ready (流式上送, 每段即发).
        Default: process() once, yield its whole result. Override for true streaming."""
        yield self.process(call_id, pcm_i16, sample_rate, channels)

    def end_call(self, call_id: str) -> None:
        """Per-call cleanup hook. Default no-op; stateful pipelines override."""
        return None


class StubPipeline(Pipeline):
    """Beep back a short tone — proves the round-trip without any external service.

    The tone's pitch tracks utterance duration so it's visibly "responding": longer
    speech → lower beep. Swap for a real pipeline in production.
    """

    def process(self, call_id: str, pcm_i16: bytes, sample_rate: int, channels: int) -> bytes:
        dur_s = len(pcm_i16) / (2 * channels * sample_rate)  # bytes→seconds (S16LE)
        # Longer utterance → lower pitch (440 Hz down to ~220 Hz over ~3 s of speech).
        freq = max(220.0, 440.0 - 70.0 * dur_s)
        return _tone(freq=freq, dur_s=0.25, rate=sample_rate, channels=channels, amp=0.35)


# ── real pipeline skeleton (TODO: implement against your LLM/TTS provider) ────
#
# The Rust ai-agent-seat uses `pipeline_mode=audio_llm` (multimodal): the utterance
# PCM is POSTed to the Doubao Responses API (ark.cn-beijing.volces.com), the LLM
# replies with a `speak(text)` tool call, and Volcano TTS (`seed-tts-2.0`, a
# bidirectional websocket) synthesizes the text to PCM. To match:
#   1. ASR-via-LLM: POST {audio: base64(utterance)} to Doubao Responses → response
#      text (parse the `speak` tool call). Keys: llm_base_url/llm_key/llm_model
#      from ai-agent-seat.conf.xml.
#   2. TTS: open a Volcano TTS websocket (wss://openspeech.bytedance.com/api/v3/tts/
#      bidirection), send the text (volcano_resource_id/volcano_speaker), receive
#      PCM chunks. Port tts_ws_codec.rs + providers/volcano_tts.rs to Python
#      (websocket + the Volcano framing protocol).
# `asr_llm_tts` (whisper ASR → text Doubao → Volcano TTS) is the simpler alternative.


def _tone(freq: float, dur_s: float, rate: int, channels: int = 1, amp: float = 0.3) -> bytes:
    n = int(dur_s * rate)
    mono = b"".join(
        struct.pack("<h", int(amp * 32767 * math.sin(2 * math.pi * freq * i / rate)))
        for i in range(n)
    )
    if channels == 1:
        return mono
    # interleave the mono frame across channels
    return b"".join(mono[j : j + 2] * channels for j in range(0, len(mono), 2))
