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
import threading

try:  # package mode (python3 -m brain.brain)
    from .esl import ESLServer, ESLSession, ESLError
    from .pipeline import Pipeline, StubPipeline
except ImportError:  # script mode (python3 brain/brain.py)
    import os
    import sys

    sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    from brain.esl import ESLServer, ESLSession, ESLError  # type: ignore
    from brain.pipeline import Pipeline, StubPipeline  # type: ignore

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
    """Per-call handler. The dialplan runs ``fswtch_vad_detect`` (attaches a
    READ_REPLACE+WRITE_REPLACE media bug) then ``socket async full`` which connects
    us AND auto-parks (driving the read/write frame loop). No ``sendmsg park``,
    no ``sendmsg bridge``, no endpoint — the park loop drives the bug directly."""
    try:
        ch = session.read_channel_data()
        a_uuid = ch.get("Unique-ID", "")
        log.info("call connected: A-leg %s", a_uuid or "?")

        # Answer + subscribe. `socket async full` already entered switch_ivr_park
        # after we sent `connect` — the park loop is driving media now.
        session.send_cmd(f"api uuid_answer {a_uuid}")
        session.send_cmd("event plain ALL")

        # Event loop — per-call, no global state.

        # 3. Event loop — per-call, no global state.
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
        log.info("call ended: A-leg %s", a_uuid or "?")


def _on_vad(headers: dict[str, str], cancel: threading.Event) -> None:
    uuid = headers.get("Call-UUID", "")
    state = headers.get("Vad-State", "")
    if not uuid or not state:
        return
    if state == "start-talking":
        # Barge-in: cancel any in-flight TTS pipeline for this call.
        cancel.set()
        log.info("start-talking  %s", uuid)
    elif state == "stop-talking":
        log.info("stop-talking   %s  (segment follows)", uuid)


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
        tts = pipeline.process(pcm, rate, channels)
        if cancel.is_set():
            log.info("pipeline cancelled (barge-in) before send on %s", uuid)
            return
        session.send_pcm(DOWNLINK, uuid, tts, rate, channels)
        log.info("downlink_pcm  %s  → %d bytes TTS to caller", uuid, len(tts))
    except Exception:
        log.exception("pipeline failed on %s", uuid)


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
        choices=("stub",),
        default="stub",
        help="business pipeline (stub=beep back; real Doubao/Volcano TODO — see pipeline.py)",
    )
    args = p.parse_args()
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s %(name)s %(levelname)s %(message)s",
    )
    # TODO: add a real pipeline (audio_llm: Doubao Responses + Volcano TTS) behind --pipeline.
    pipeline: Pipeline = StubPipeline()
    serve(args.host, args.port, pipeline)


if __name__ == "__main__":
    main()
