"""Python brain for the fswtch VAD module (`mod_vad_bot`).

Re-implements ai-agent-seat's role (the business / brain) as an external ESL client,
fully decoupled from the voice media path. The VAD module (mod_vad_bot, an FS
endpoint) does VAD + media; this does the brain. They talk ONLY via ESL events:

    fswtch::vad         (in)   VAD→brain: start-talking / stop-talking (no body, `Seq`)
    fswtch::uplink_pcm  (in)   VAD→brain: caller PCM, base64 body, `Seq` (same Seq as the
                               matching `vad` event on the start/stop frame)
    fswtch::downlink_pcm(out)  brain→VAD: TTS PCM, base64 body + Target-UUID → played to caller

Flow per utterance: start-talking → (uplink_pcm frames) → stop-talking → the brain
concatenates the buffered PCM (Seq in [start, stop]) → runs the pipeline (ASR/LLM/TTS)
→ sends the TTS PCM back as downlink_pcm. Barge-in: a new start-talking cancels any
in-flight pipeline for that call (the VAD module also flushes its own play queue).

Run:
    python3 -m brain.brain                          # from the repo root
    python3 brain/brain.py                          # or as a script
    python3 -m brain.brain --host 127.0.0.1 --port 8022 --password ClueCon
"""

from __future__ import annotations

import argparse
import base64
import logging
import threading

try:  # package mode (python3 -m brain.brain)
    from .esl import ESLClient
    from .pipeline import Pipeline, StubPipeline
except ImportError:  # script mode (python3 brain/brain.py)
    import os
    import sys

    sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    from brain.esl import ESLClient  # type: ignore
    from brain.pipeline import Pipeline, StubPipeline  # type: ignore

log = logging.getLogger("brain")

VAD = "fswtch::vad"
UPLINK = "fswtch::uplink_pcm"
DOWNLINK = "fswtch::downlink_pcm"


class _Call:
    __slots__ = ("start_seq", "chunks", "cancel", "rate", "channels")

    def __init__(self) -> None:
        self.start_seq: int | None = None
        self.chunks: list[tuple[int, bytes]] = []
        self.cancel = threading.Event()
        self.rate = 8000
        self.channels = 1


class Brain:
    """Glue between the ESL event stream and the business `Pipeline`."""

    def __init__(self, esl: ESLClient, pipeline: Pipeline) -> None:
        self.esl = esl
        self.pipeline = pipeline
        self.calls: dict[str, _Call] = {}
        self._lock = threading.Lock()

    def run(self) -> None:
        log.info("brain running; listening for %s / %s", VAD, UPLINK)
        while True:
            headers, body = self.esl.recv_event()
            sub = headers.get("Event-Subclass")
            if sub == VAD:
                self._on_vad(headers)
            elif sub == UPLINK:
                self._on_uplink(headers, body)

    def _on_vad(self, headers: dict[str, str]) -> None:
        uuid = headers.get("Call-UUID", "")
        state = headers.get("Vad-State", "")
        seq = _int(headers.get("Seq"))
        if not uuid or seq is None or not state:
            return
        with self._lock:
            call = self.calls.get(uuid)
            if state == "start-talking":
                # barge-in: cancel any in-flight TTS pipeline for this call.
                if call is not None:
                    call.cancel.set()
                call = _Call()
                call.start_seq = seq
                self.calls[uuid] = call
                log.info("start-talking  %s  seq=%d", uuid, seq)
            elif state == "stop-talking":
                if call is None or call.start_seq is None:
                    return
                # The VAD module fires uplink_pcm before vad on each frame, so the
                # stop frame's PCM (Seq==seq) is already buffered.
                chunks = [c for c in call.chunks if call.start_seq <= c[0] <= seq]
                chunks.sort(key=lambda c: c[0])
                pcm = b"".join(c[1] for c in chunks)
                rate, channels, cancel = call.rate, call.channels, call.cancel
                call.chunks = []  # reset for the next utterance on this call
                call.start_seq = None
                log.info(
                    "stop-talking   %s  seq=%d  → %d bytes PCM (%d Hz) → pipeline",
                    uuid, seq, len(pcm), rate,
                )
                threading.Thread(
                    target=self._run_pipeline,
                    args=(uuid, pcm, rate, channels, cancel),
                    name=f"brain-{uuid[:8]}",
                    daemon=True,
                ).start()

    def _on_uplink(self, headers: dict[str, str], body: bytes) -> None:
        uuid = headers.get("Call-UUID", "")
        seq = _int(headers.get("Seq"))
        if not uuid or seq is None:
            return
        rate = _int(headers.get("Sample-Rate")) or 8000
        channels = _int(headers.get("Channels")) or 1
        try:
            pcm = base64.b64decode(body)
        except Exception as e:
            log.warning("uplink_pcm decode failed on %s: %s", uuid, e)
            return
        with self._lock:
            call = self.calls.get(uuid)
            if call is None:
                call = _Call()
                self.calls[uuid] = call
            call.rate = rate
            call.channels = channels
            call.chunks.append((seq, pcm))

    def _run_pipeline(
        self,
        uuid: str,
        pcm: bytes,
        rate: int,
        channels: int,
        cancel: threading.Event,
    ) -> None:
        try:
            tts = self.pipeline.process(pcm, rate, channels)
            if cancel.is_set():
                log.info("pipeline cancelled (barge-in) before send on %s", uuid)
                return
            self.esl.send_pcm(DOWNLINK, uuid, tts, rate, channels)
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
    p = argparse.ArgumentParser(description="fswtch VAD-module Python brain")
    p.add_argument("--host", default="127.0.0.1")
    p.add_argument("--port", type=int, default=8022)
    p.add_argument("--password", default="ClueCon")
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
    esl = ESLClient(args.host, args.port, args.password)
    brain = Brain(esl, pipeline)
    try:
        brain.run()
    except KeyboardInterrupt:
        log.info("shutting down")
    finally:
        esl.close()


if __name__ == "__main__":
    main()
