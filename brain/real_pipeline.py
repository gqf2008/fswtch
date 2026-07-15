"""Real asr_llm_tts pipeline for the Python brain.

Mirrors the vox-seat `mod_voice_seat` contract (config: `voice_seat.conf.xml`):

  caller PCM (8 kHz mono S16LE)
    → Volcano bigmodel ASR (flash, HTTP)            → text
    → DeepSeek LLM (OpenAI-compatible /chat/completions, non-stream) → reply text
    → Volcano bidirectional WebSocket TTS (one-shot, request 8 kHz) → S16LE bytes
    → returned to the brain, which fires `fswtch::downlink_pcm`

Everything stays at 8 kHz mono S16LE — `mod_vad_detect` plays the downlink queue at
its fixed 8 kHz L16 codec and ignores the `Sample-Rate` header, so no resampling is
needed (TTS is requested at 8 kHz; ASR receives an 8 kHz WAV).

v1 limitations (see plan): non-streaming LLM + one-shot TTS (full-utterance latency),
no tool execution (hangup/transfer/dtmf), per-call history (multi-turn within a call).
"""

from __future__ import annotations

import base64
import json
import logging
import array
import struct
import urllib.error
import urllib.request
import uuid
import wave
import io
from dataclasses import dataclass, field
from xml.etree import ElementTree

from .pipeline import Pipeline

log = logging.getLogger("brain.pipeline")

DEFAULT_TTS_URL = "wss://openspeech.bytedance.com/api/v3/tts/bidirection"
DEFAULT_ASR_URL = "https://openspeech.bytedance.com/api/v3/auc/bigmodel/recognize/flash"
TTS_SAMPLE_RATE = 8000  # match mod_vad_detect's 8 kHz playback → no resampling

# ── Volcano bidirectional-WS protocol constants ──────────────────────────────
# 4-byte header: (version<<4)|hdr_size=0x11, (msg_type<<4)|flag, (ser<<4)|compress=0x10, 0x00
MSG_FULL_CLIENT = 0x1
FLAG_WITH_EVENT = 0x4
MSG_FULL_SERVER = 0x9
MSG_AUDIO_ONLY_SERVER = 0xB
MSG_ERROR = 0xF
FLAG_POSITIVE_SEQ = 0x1

# connection-level events that OMIT the session_id field on the wire
_SKIP_SESSION = {1, 2, 50, 51}  # writer; 52 only on the reader side
_READER_SKIP_SESSION = {1, 2, 50, 51, 52}
_CARRIES_CONNECT_ID = {50, 51, 52}

E_START_CONNECTION = 1
E_FINISH_CONNECTION = 2
E_START_SESSION = 100
E_CANCEL_SESSION = 101
E_FINISH_SESSION = 102
E_SESSION_STARTED = 150
E_SESSION_FINISHED = 152
E_SESSION_FAILED = 153
E_TTS_SENTENCE_START = 350
E_TTS_SENTENCE_END = 351


# ── config ───────────────────────────────────────────────────────────────────

@dataclass
class RealConfig:
    llm_base_url: str = ""
    llm_key: str = ""
    llm_model: str = ""
    llm_temperature: float | None = None
    llm_max_tokens: int | None = None
    llm_reasoning_effort: str = ""
    volcano_tts_url: str = ""
    volcano_api_key: str = ""
    volcano_resource_id: str = ""
    volcano_speaker: str = ""
    volcano_asr_url: str = ""
    volcano_asr_key: str = ""
    volcano_asr_resource_id: str = ""
    volcano_asr_enable_punc: bool = True
    system_prompt: str = ""


def _conf_path() -> str:
    return "/Users/sqb/.local/etc/freeswitch/autoload_configs/voice_seat.conf.xml"


def load_config() -> RealConfig:
    """Parse `voice_seat.conf.xml` `<settings><param name= value=/>` → RealConfig."""
    cfg = RealConfig()
    try:
        root = ElementTree.parse(_conf_path()).getroot()
    except Exception as e:
        log.error("failed to parse %s: %s", _conf_path(), e)
        return cfg
    for param in root.iter("param"):
        name = (param.get("name") or "").strip()
        value = param.get("value") or ""
        match name:
            case "llm_base_url": cfg.llm_base_url = value
            case "llm_key": cfg.llm_key = value
            case "llm_model": cfg.llm_model = value
            case "llm_temperature": cfg.llm_temperature = _f(value)
            case "llm_max_tokens": cfg.llm_max_tokens = _i(value)
            case "llm_reasoning_effort": cfg.llm_reasoning_effort = value
            case "volcano_tts_url": cfg.volcano_tts_url = value
            case "volcano_api_key": cfg.volcano_api_key = value
            case "volcano_resource_id": cfg.volcano_resource_id = value
            case "volcano_speaker": cfg.volcano_speaker = value
            case "volcano_asr_url": cfg.volcano_asr_url = value
            case "volcano_asr_key": cfg.volcano_asr_key = value
            case "volcano_asr_resource_id": cfg.volcano_asr_resource_id = value
            case "volcano_asr_enable_punc": cfg.volcano_asr_enable_punc = value in ("true", "1")
            case "system_prompt_file":
                try:
                    with open(value, "r", encoding="utf-8") as f:
                        cfg.system_prompt = f.read()
                except Exception as e:
                    log.warning("system_prompt_file %s read failed: %s", value, e)
    log.info(
        "real config: llm=%s model=%s asr=%s tts=%s speaker=%s",
        cfg.llm_base_url or "(none)", cfg.llm_model or "(none)",
        "volcano_bigmodel" if cfg.volcano_asr_key else "(none)",
        "volcano_ws" if cfg.volcano_api_key else "(none)",
        cfg.volcano_speaker or "(none)",
    )
    return cfg


def _f(s: str) -> float | None:
    try: return float(s)
    except (TypeError, ValueError): return None

def _i(s: str) -> int | None:
    try: return int(s)
    except (TypeError, ValueError): return None


# ── ASR: Volcano bigmodel flash (HTTP) ───────────────────────────────────────

def _wav_pcm(pcm: bytes, rate: int, channels: int) -> bytes:
    """Wrap raw S16LE into a 44-byte-header WAV container."""
    buf = io.BytesIO()
    with wave.open(buf, "wb") as w:
        w.setnchannels(channels)
        w.setsampwidth(2)  # 16-bit
        w.setframerate(rate)
        w.writeframes(pcm)
    return buf.getvalue()


def _resample_linear(pcm: bytes, in_rate: int, out_rate: int) -> tuple[bytes, int]:
    """Linear-interpolation resample of mono S16LE PCM. Returns (pcm, rate).

    bigmodel flash is tuned for 16 kHz (mod_voice_seat sends native 16k); mod_vad_detect
    delivers 8 kHz, so we upsample telephony audio to 16 kHz before ASR. Linear interp is
    fine for 2x upsampling (no aliasing when upsampling). Pure-Python (~0.1s for a 9s clip).
    """
    if in_rate == out_rate or in_rate == 0 or not pcm:
        return pcm, in_rate
    n = len(pcm) // 2
    src = array.array("h")
    src.frombytes(pcm[: n * 2])  # native-endian; macOS arm64 is little → matches S16LE
    factor = out_rate / in_rate
    out_n = int(round(n * factor))
    res = [0] * out_n
    last = n - 1
    for i in range(out_n):
        pos = i / factor
        j = int(pos)
        if j >= last:
            res[i] = src[last] if n else 0
        else:
            frac = pos - j
            v = int(src[j] + (src[j + 1] - src[j]) * frac)
            res[i] = -32768 if v < -32768 else 32767 if v > 32767 else v
    return array.array("h", res).tobytes(), out_rate


class VolcanoAsr:
    """Volcano bigmodel ASR `recognize/flash` — one HTTP POST, returns transcript."""

    def __init__(self, cfg: RealConfig) -> None:
        self.url = cfg.volcano_asr_url or DEFAULT_ASR_URL
        self.key = cfg.volcano_asr_key
        self.resource_id = cfg.volcano_asr_resource_id
        self.enable_punc = cfg.volcano_asr_enable_punc

    def transcribe(self, pcm: bytes, rate: int, channels: int) -> str:
        pcm, rate = _resample_linear(pcm, rate, 16000)
        wav = _wav_pcm(pcm, rate, channels)
        body = json.dumps({
            "user": {"uid": "voice_seat"},
            "audio": {"data": base64.b64encode(wav).decode()},
            "request": {"model_name": "bigmodel", "enable_punc": self.enable_punc},
        }).encode()
        req = urllib.request.Request(self.url, data=body, method="POST", headers={
            "X-Api-Key": self.key,
            "X-Api-Resource-Id": self.resource_id,
            "X-Api-Request-Id": str(uuid.uuid4()),
            "X-Api-Sequence": "-1",
            "Content-Type": "application/json",
        })
        try:
            with urllib.request.urlopen(req, timeout=15) as resp:
                status = resp.headers.get("X-Api-Status-Code", "")
                payload = resp.read()
        except urllib.error.HTTPError as e:
            try: err_body = e.read()[:500]
            except Exception: err_body = b""
            log.error("ASR HTTP %d: %r", e.code, err_body)
            raise RuntimeError(f"ASR HTTP {e.code}: {err_body!r}") from e
        if status == "20000000":
            text = json.loads(payload).get("result", {}).get("text", "").strip()
            log.info("ASR ok: %r", text)
            return text
        if status in ("20000003", "45000002"):
            log.info("ASR empty audio (status=%s) → no speech", status)
            return ""
        log.error("ASR error status=%s body=%r", status, payload[:500])
        raise RuntimeError(f"ASR failed: status={status} body={payload[:500]!r}")


# ── LLM: DeepSeek / OpenAI-compatible chat/completions (non-stream) ─────────

class DeepSeekLlm:
    """OpenAI-compatible /chat/completions, non-streaming. Reply = concatenated text."""

    def __init__(self, cfg: RealConfig) -> None:
        self.base_url = cfg.llm_base_url.rstrip("/")
        self.key = cfg.llm_key
        self.model = cfg.llm_model
        self.temperature = cfg.llm_temperature
        self.max_tokens = cfg.llm_max_tokens
        self.reasoning_effort = cfg.llm_reasoning_effort

    def chat(self, system: str, history: list[dict], user_text: str) -> str:
        messages: list[dict] = []
        if system:
            messages.append({"role": "system", "content": system})
        messages.extend(history)
        messages.append({"role": "user", "content": user_text})
        body = self._build_body(messages)
        try:
            return self._post(body)
        except urllib.error.HTTPError as e:
            # Some providers reject `reasoning_effort`; retry without it.
            if e.code == 400 and self.reasoning_effort:
                log.warning("LLM 400 with reasoning_effort=%r, retrying without", self.reasoning_effort)
                return self._post(self._build_body(messages, reasoning=False))
            raise

    def _build_body(self, messages: list[dict], reasoning: bool = True) -> bytes:
        body: dict = {
            "model": self.model,
            "messages": messages,
            "stream": False,
        }
        if self.temperature is not None: body["temperature"] = self.temperature
        if self.max_tokens is not None: body["max_tokens"] = self.max_tokens
        if reasoning and self.reasoning_effort:
            body["reasoning_effort"] = self.reasoning_effort
        return json.dumps(body).encode()

    def _post(self, body: bytes) -> str:
        req = urllib.request.Request(
            f"{self.base_url}/chat/completions", data=body, method="POST", headers={
                "Authorization": f"Bearer {self.key}",
                "Content-Type": "application/json",
            })
        with urllib.request.urlopen(req, timeout=30) as resp:
            data = json.loads(resp.read())
        choice = (data.get("choices") or [{}])[0]
        msg = choice.get("message", {})
        content = (msg.get("content") or "").strip()
        finish = choice.get("finish_reason")
        log.info("LLM reply (%d chars) finish=%s: %r", len(content), finish, content)
        return content


# ── TTS: Volcano bidirectional WebSocket (one-shot) ──────────────────────────

def _marshal_control(event: int, session_id: str, payload: dict) -> bytes:
    """Build a client→server FullClientRequest|WithEvent frame."""
    out = bytearray([0x11, (MSG_FULL_CLIENT << 4) | FLAG_WITH_EVENT, 0x10, 0x00])
    out += struct.pack(">i", event)
    if event not in _SKIP_SESSION:
        sid = session_id.encode()
        out += struct.pack(">I", len(sid)) + sid
    payload_bytes = json.dumps(payload, separators=(",", ":")).encode()
    out += struct.pack(">I", len(payload_bytes)) + payload_bytes
    return bytes(out)


def _parse_frame(data: bytes) -> dict:
    """Parse one server→client Volcano frame. Returns dict with parsed fields."""
    b1 = data[1]
    msg_type = b1 >> 4
    flag = b1 & 0x0F
    pos = 4
    seq = err = event = None
    sid = cid = b""
    if msg_type in (0x1, 0x9, 0xC, 0x2, 0xB) and flag in (0x1, 0x3):
        seq = struct.unpack(">i", data[pos:pos + 4])[0]; pos += 4
    elif msg_type == 0xF:
        err = struct.unpack(">I", data[pos:pos + 4])[0]; pos += 4
    if flag == FLAG_WITH_EVENT:
        event = struct.unpack(">i", data[pos:pos + 4])[0]; pos += 4
        if event not in _READER_SKIP_SESSION:
            slen = struct.unpack(">I", data[pos:pos + 4])[0]; pos += 4
            sid = data[pos:pos + slen]; pos += slen
        if event in _CARRIES_CONNECT_ID:
            clen = struct.unpack(">I", data[pos:pos + 4])[0]; pos += 4
            cid = data[pos:pos + clen]; pos += clen
    plen = struct.unpack(">I", data[pos:pos + 4])[0]; pos += 4
    payload = data[pos:pos + plen]
    return {"msg_type": msg_type, "flag": flag, "seq": seq, "error": err,
            "event": event, "sid": sid, "cid": cid, "payload": payload}


class VolcanoTts:
    """Volcano bidirectional TTS used one-shot: synth one text → all 8 kHz PCM bytes."""

    def __init__(self, cfg: RealConfig) -> None:
        self.url = cfg.volcano_tts_url or DEFAULT_TTS_URL
        self.key = cfg.volcano_api_key
        self.resource_id = cfg.volcano_resource_id
        self.speaker = cfg.volcano_speaker

    def synth_stream(self, text: str):
        """Stream TTS PCM chunks (S16LE @8kHz) as they arrive from Volcano — 流式上送。
        Yields bytes chunks; caller sends each as a downlink_pcm immediately instead of
        buffering the whole utterance (低首字延迟 + barge-in 可中途停发后续 chunk)。"""
        import websocket  # local import: websocket-client is a hard dep for real pipeline

        connect_id = str(uuid.uuid4())
        session_id = str(uuid.uuid4())
        log.info("TTS connect: %s sid=%s", self.url, session_id[:8])
        ws = websocket.create_connection(
            self.url,
            header=[
                f"X-Api-Key: {self.key}",
                f"X-Api-Resource-Id: {self.resource_id}",
                f"X-Api-Connect-Id: {connect_id}",
            ],
            timeout=20,
        )
        try:
            ws.send_binary(_marshal_control(E_START_CONNECTION, session_id, {}))
            ws.send_binary(_marshal_control(E_START_SESSION, session_id, {
                "req_params": {
                    "speaker": self.speaker,
                    "audio_params": {"format": "pcm", "sample_rate": TTS_SAMPLE_RATE},
                    "section_id": session_id,
                },
            }))
            log.info("TTS start_session sent, waiting SessionStarted")
            self._wait_event(ws, E_SESSION_STARTED, "SessionStarted")
            log.info("TTS SessionStarted, sending task_request")
            ws.send_binary(_marshal_control(200, session_id, {  # task_request
                "req_params": {
                    "text": text,
                    "speaker": self.speaker,
                    "audio_params": {"format": "pcm", "sample_rate": TTS_SAMPLE_RATE},
                },
            }))
            total = 0
            for chunk in self._iter_audio(ws, session_id):
                total += len(chunk)
                yield chunk
            log.info("TTS synth_stream done: %d bytes PCM (%.1fs)",
                     total, total / (2 * TTS_SAMPLE_RATE))
            try:
                ws.send_binary(_marshal_control(E_FINISH_CONNECTION, session_id, {}))
            except Exception:
                pass
        finally:
            # 取消 Volcano 在途合成（brain 中途 close 生成器 = barge-in/挂断；正常结束也幂等）
            try:
                ws.send_binary(_marshal_control(E_CANCEL_SESSION, session_id, {}))
            except Exception:
                pass
            try: ws.close()
            except Exception: pass

    def synth(self, text: str) -> bytes:
        """One-shot: buffer the whole utterance (back-compat, 整句一次发)."""
        return b"".join(self.synth_stream(text))

    def _wait_event(self, ws, want_event: int, label: str) -> None:
        import websocket as _ws_mod
        while True:
            raw = ws.recv()
            if isinstance(raw, str):
                continue  # unexpected text frame
            frame = _parse_frame(raw)
            ev = frame["event"]
            if frame["msg_type"] == MSG_ERROR or ev in (E_SESSION_FAILED, 153):
                raise RuntimeError(f"{label} not reached: error={frame.get('error')} event={ev}")
            if ev == want_event:
                return

    def _iter_audio(self, ws, session_id: str):
        """Yield TTS PCM chunks as they arrive (流式). Handles idle-detect + finish_session
        to elicit the terminal TTSSentenceEnd(351)."""
        import websocket
        saw_audio = False
        finished = False
        n = 0
        ws.settimeout(1.5)  # idle-detect during streaming; audio goes quiet → sentence ended
        while True:
            try:
                raw = ws.recv()
            except websocket.WebSocketTimeoutException:
                if not saw_audio:
                    continue  # audio not started yet; keep waiting for first chunk
                if not finished:
                    # audio stream went idle → tell server the turn is done so it
                    # emits the terminal TTSSentenceEnd(351) (it won't otherwise).
                    log.info("TTS audio idle after %d frames → finish_session", n)
                    try:
                        ws.send_binary(_marshal_control(E_FINISH_SESSION, session_id, {}))
                    except Exception:
                        pass
                    finished = True
                    ws.settimeout(5.0)  # give the terminal event time to arrive
                    continue
                return  # already finished and idle again → accept what we have
            if isinstance(raw, str):
                continue
            n += 1
            frame = _parse_frame(raw)
            mt, fl, ev = frame["msg_type"], frame["flag"], frame["event"]
            plen = len(frame["payload"])
            if n <= 8 or n % 50 == 0:
                log.info("TTS frame#%d: mt=0x%X fl=0x%X event=%s plen=%d", n, mt, fl, ev, plen)
            # Audio frames: mt=AudioOnlyServer(0xB); the live server uses flag=WithEvent(0x4),
            # event=TTSResponse(352), payload = raw S16LE PCM. Yield each 0xB payload.
            if mt == MSG_AUDIO_ONLY_SERVER:
                if plen >= 2:
                    yield frame["payload"]
                saw_audio = True
            elif mt == MSG_FULL_SERVER and fl == FLAG_WITH_EVENT and ev == E_TTS_SENTENCE_END:
                log.info("TTS TTSSentenceEnd(351) at frame#%d", n)
                return
            elif mt == MSG_FULL_SERVER and fl == FLAG_WITH_EVENT and ev in (E_SESSION_FINISHED, E_SESSION_FAILED):
                log.info("TTS session end event=%s at frame#%d", ev, n)
                return
            elif mt == MSG_ERROR:
                raise RuntimeError(f"TTS server error: code={frame.get('error')}")
            # TTSSentenceStart(350) and other control events: ignore, keep streaming.


# ── orchestrating pipeline ──────────────────────────────────────────────────

class AsrLlmTtsPipeline(Pipeline):
    """asr_llm_tts: ASR → DeepSeek LLM → Volcano TTS, per-call history."""

    def __init__(self, cfg: RealConfig) -> None:
        self.cfg = cfg
        self.asr = VolcanoAsr(cfg)
        self.llm = DeepSeekLlm(cfg)
        self.tts = VolcanoTts(cfg)
        self.history: dict[str, list[dict]] = {}

    def process(self, call_id: str, pcm_i16: bytes, sample_rate: int, channels: int) -> bytes:
        # One-shot (整句): buffer the streamed reply. brain uses process_stream directly.
        return b"".join(self.process_stream(call_id, pcm_i16, sample_rate, channels))

    def process_stream(self, call_id: str, pcm_i16: bytes, sample_rate: int, channels: int):
        """ASR → DeepSeek LLM → Volcano TTS, yielding TTS PCM chunks as they arrive (流式上送)."""
        text = self.asr.transcribe(pcm_i16, sample_rate, channels)
        if not text:
            return  # no speech recognized → no reply (empty generator)
        hist = self.history.setdefault(call_id, [])  # prior turns (excl. current)
        reply = self.llm.chat(self.cfg.system_prompt, list(hist), text)
        if not reply:
            return
        hist.append({"role": "user", "content": text})
        hist.append({"role": "assistant", "content": reply})
        log.info("TTS streaming reply (%d chars) → downlink chunks", len(reply))
        yield from self.tts.synth_stream(reply)

    def end_call(self, call_id: str) -> None:
        self.history.pop(call_id, None)
