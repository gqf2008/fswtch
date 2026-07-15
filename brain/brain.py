"""Python brain for the fswtch VAD module (`mod_vad_bot`) — **outbound ESL** mode.

The brain is a TCP server; FreeSWITCH connects to it per-call via the dialplan
``socket`` application. Each call gets its own connection and handler thread:

1. FS connects → brain reads channel data (A-leg ``Unique-ID``, APM vars).
2. Brain subscribes to events, parks the call, and bridges to
   ``fswtch_vad_bot/1000`` (the VAD endpoint that becomes the B-leg).
3. ``mod_vad_bot`` runs VAD locally + ferries audio as ESL events:
   - ``fswtch::vad``         (VAD→brain) start-talking / stop-talking (no body, ``Seq``)
   - ``fswtch::uplink_pcm``  (VAD→brain) caller PCM, base64 body, ``Seq``
   - ``fswtch::downlink_pcm``(brain→VAD) TTS PCM, base64 body + Target-UUID
4. Brain buffers the utterance, runs the pipeline (ASR/LLM/TTS), sends TTS back.
   Barge-in: a new start-talking cancels the in-flight pipeline for that call.

Run::

    python3 -m brain.brain                          # from the repo root
    python3 brain/brain.py                          # or as a script
    python3 -m brain.brain --host 127.0.0.1 --port 8084

Dialplan::

    <action application="export" data="FSWTCH_NS=12"/>
    <action application="socket" data="127.0.0.1:8084 full"/>
"""

from __future__ import annotations

import argparse
import base64
import logging
import os
import struct
import threading

try:  # package mode (python3 -m brain.brain)
    from .esl import ESLServer, ESLSession, ESLError
    from .pipeline import Pipeline, StubPipeline
    from .real_pipeline import AsrLlmTtsPipeline, load_config
except ImportError:  # script mode (python3 brain/brain.py)
    import os
    import sys

    sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    from brain.esl import ESLServer, ESLSession, ESLError  # type: ignore
    from brain.pipeline import Pipeline, StubPipeline  # type: ignore
    from brain.real_pipeline import AsrLlmTtsPipeline, load_config  # type: ignore

log = logging.getLogger("brain")

VAD = "fswtch::vad"
UPLINK = "fswtch::uplink_pcm"
DOWNLINK = "fswtch::downlink_pcm"


def serve(host: str, port: int, pipeline: Pipeline) -> None:
    """Listen for outbound ESL connections; spawn one handler per call."""
    server = ESLServer(host, port)
    log.info("brain listening on %s:%d", host, port)
    while True:
        try:
            session = server.accept()
        except OSError:
            continue  # accept failed; retry
        threading.Thread(
            target=handle_call,
            args=(session, pipeline),
            name=f"brain-{session.addr[0]}:{session.addr[1]}",
            daemon=True,
        ).start()


def handle_call(session: ESLSession, pipeline: Pipeline) -> None:
    """Per-call handler. ``socket async full`` connects us AND auto-parks.
    We send ``sendmsg bridge fswtch_vad_detect/1000`` — the park loop's
    ``switch_ivr_parse_all_events`` processes it, creating a B-leg (endpoint).
    The bridge drives both ``write_frame`` (caller audio → VAD → fire events)
    and ``read_frame`` (drain TTS → caller). No media bug, no playback, no
    park CNG issue."""
    a_uuid = ""
    try:
        ch = session.read_channel_data()
        a_uuid = ch.get("Unique-ID", "")
        log.info("call connected: A-leg %s", a_uuid or "?")

        # Answer + subscribe + bridge. `socket async full` already entered
        # switch_ivr_park after we sent `connect` — the park loop processes
        # our sendmsg commands via switch_ivr_parse_all_events.
        session.send_cmd(f"api uuid_answer {a_uuid}")
        session.send_cmd("event plain ALL")
        reply = session.send_app("bridge", "fswtch_vad_detect/1000")
        if not reply.get("Reply-Text", "").startswith("+OK"):
            log.error("bridge failed on %s: %s", a_uuid, reply.get("Reply-Text"))
            return

        # Event loop — per-call, no global state.
        cancel = threading.Event()
        b_uuid = ""  # B-leg Call-UUID (learned from the first VAD event).

        while True:
            try:
                headers, body = session.recv_event()
            except (ESLError, OSError):
                break  # socket closed/reset = call ended (normal hangup)

            sub = headers.get("Event-Subclass")
            if sub == VAD:
                _on_vad(headers, cancel)
            elif sub == UPLINK:
                uuid = headers.get("Call-UUID", "")
                if not b_uuid:
                    b_uuid = uuid  # first uplink/vad event tells us the B-leg
                if b_uuid and uuid != b_uuid:
                    continue  # multi-call: not this handler's call
                _on_uplink(headers, body, session, pipeline, cancel)
    except ESLError:
        pass  # socket closed mid-stream
    except Exception:
        log.exception("handler crashed")
    finally:
        session.close()
        if a_uuid:
            pipeline.end_call(a_uuid)
        log.info("call ended: A-leg %s", a_uuid or "?")


def _on_vad(headers: dict[str, str], cancel: threading.Event) -> None:
    uuid = headers.get("Call-UUID", "")
    state = headers.get("Vad-State", "")
    seq = headers.get("Seq", "")
    if not uuid or not state:
        return
    tag = uuid[:8]
    if state == "start-talking":
        # Barge-in: cancel any in-flight TTS pipeline for this call.
        cancel.set()
        print(f"[VAD] start-talking (barge-in) uuid={tag} seq={seq}", flush=True)
    elif state == "stop-talking":
        print(f"[VAD] stop-talking (end)      uuid={tag} seq={seq} → segment", flush=True)
    else:
        print(f"[VAD] state={state} uuid={tag} seq={seq}", flush=True)
    log.info("vad state=%s uuid=%s seq=%s", state, uuid, seq)


def _on_uplink(
    headers: dict[str, str],
    body: bytes,
    session: ESLSession,
    pipeline: Pipeline,
    cancel: threading.Event,
) -> None:
    uuid = headers.get("Call-UUID", "")
    if not uuid:
        return
    rate = _int(headers.get("Sample-Rate")) or 8000
    channels = _int(headers.get("Channels")) or 1
    try:
        pcm = base64.b64decode(body)
    except Exception as e:
        log.warning(
            "uplink_pcm decode failed on %s: %s (body_len=%d head=%r tail=%r)",
            uuid, e, len(body),
            body[:16] if body else b"", body[-16:] if body else b"",
        )
        return
    log.info(
        "uplink_pcm segment  %s  → %d bytes PCM (%d Hz) → pipeline",
        uuid, len(pcm), rate,
    )
    # Audio-level diagnostic: peak/RMS tells empty-ASR segments (silent=VAD
    # false-fire on noise) from real speech ASR missed. Also dump the segment
    # to /tmp/uplink_segs/ for offline inspection.
    n = len(pcm) // 2
    samples = struct.unpack(f"<{n}h", pcm[: n * 2]) if n else ()
    peak = max((abs(s) for s in samples), default=0)
    rms = (sum(s * s for s in samples) / n) ** 0.5 if n else 0.0
    seq = headers.get("Seq", "")
    print(
        f"[UPLINK] segment uuid={uuid[:8]} seq={seq} {len(pcm)}b {rate}Hz "
        f"peak={peak} rms={rms:.0f} → pipeline",
        flush=True,
    )
    try:
        os.makedirs("/tmp/uplink_segs", exist_ok=True)
        with open(f"/tmp/uplink_segs/seq{seq}_{len(pcm)}b_peak{peak}_rms{int(rms)}.pcm", "wb") as f:
            f.write(pcm)
    except Exception:
        pass
    # Run the pipeline + send TTS in a background thread so the event loop
    # keeps draining (barge-in can cancel before the reply is sent).
    cancel.clear()
    thread = threading.Thread(
        target=_run_pipeline,
        args=(session, uuid, pcm, rate, channels, pipeline, cancel),
        name=f"brain-{uuid[:8]}",
        daemon=True,
    )
    thread.start()


def _run_pipeline(
    session: ESLSession,
    uuid: str,
    pcm: bytes,
    rate: int,
    channels: int,
    pipeline: Pipeline,
    cancel: threading.Event,
) -> None:
    try:
        tts = pipeline.process(uuid, pcm, rate, channels)
        if cancel.is_set():
            log.info("pipeline cancelled (barge-in) before send on %s", uuid)
            print(f"[DOWNLINK] (cancelled by barge-in) uuid={uuid[:8]} → not sent", flush=True)
            return
        session.send_pcm(DOWNLINK, uuid, tts, rate, channels)
        print(
            f"[DOWNLINK] reply uuid={uuid[:8]} {len(tts)} bytes TTS → caller "
            f"({len(tts) // (2 * rate)}s)",
            flush=True,
        )
        log.info("downlink_pcm  %s  → %d bytes TTS to caller", uuid, len(tts))
    except Exception:
        log.exception("pipeline failed on %s", uuid)
        print(f"[DOWNLINK] (pipeline failed) uuid={uuid[:8]}", flush=True)


def _int(s: str | None) -> int | None:
    if not s:
        return None
    try:
        return int(s)
    except ValueError:
        return None


def main() -> None:
    p = argparse.ArgumentParser(description="fswtch VAD-module Python brain (outbound ESL)")
    p.add_argument("--host", default="127.0.0.1")
    p.add_argument("--port", type=int, default=8084)
    p.add_argument(
        "--pipeline",
        choices=("stub", "asr_llm_tts"),
        default="stub",
        help="business pipeline (stub=beep back; asr_llm_tts=Volcano ASR + DeepSeek LLM + Volcano TTS)",
    )
    args = p.parse_args()
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s %(name)s %(levelname)s %(message)s",
    )
    if args.pipeline == "asr_llm_tts":
        pipeline: Pipeline = AsrLlmTtsPipeline(load_config())
    else:
        pipeline = StubPipeline()
    serve(args.host, args.port, pipeline)


if __name__ == "__main__":
    main()
