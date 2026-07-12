"""Minimal FreeSWITCH ESL raw-socket client (zero dependency).

Speaks the ESL text protocol over a plain TCP socket — no `python-ESL` SWIG binding
needed. Connects, authenticates, subscribes to CUSTOM events, parses inbound event
blocks (headers + Content-Length body), and sends CUSTOM events (`sendevent`).

Usage:
    esl = ESLClient("127.0.0.1", 8022, "ClueCon")
    while True:
        headers, body = esl.recv_event()
        if headers.get("Event-Subclass") == "fswtch::uplink_pcm":
            ...  # body is the base64 PCM string (decode via base64.b64decode)
"""

from __future__ import annotations

import socket
import base64


class ESLError(RuntimeError):
    pass


class ESLClient:
    def __init__(self, host: str = "127.0.0.1", port: int = 8022, password: str = "ClueCon"):
        self.sock = socket.create_connection((host, port), timeout=10)
        self.sock.settimeout(None)  # blocking reads after connect
        self._buf = b""
        # FS sends `Content-Type: auth/request` immediately on connect — read + discard it.
        self._read_headers()
        self._send(f"auth {password}\n\n".encode())
        self._expect_ok()
        # Subscribe to ALL CUSTOM events; filter by Event-Subclass client-side.
        # (`event plain custom <sub>` is fs_cli sugar; the robust raw form is all-CUSTOM.)
        self._send(b"event plain CUSTOM\n\n")
        self._expect_ok()

    # ── low-level stream helpers ───────────────────────────────────────────

    def _send(self, data: bytes) -> None:
        self.sock.sendall(data)

    def _read_line(self) -> bytes:
        while b"\n" not in self._buf:
            chunk = self.sock.recv(4096)
            if not chunk:
                raise ESLError("ESL socket closed")
            self._buf += chunk
        line, self._buf = self._buf.split(b"\n", 1)
        return line

    def _read_headers(self) -> dict[str, str]:
        headers: dict[str, str] = {}
        while True:
            line = self._read_line()
            if line == b"":
                break  # blank line terminates the header block
            if b": " in line:
                k, v = line.split(b": ", 1)
                headers[k.decode(errors="replace")] = v.decode(errors="replace")
        return headers

    def _expect_ok(self) -> None:
        headers = self._read_headers()
        reply = headers.get("Reply-Text", "")
        if not reply.startswith("+OK"):
            raise ESLError(f"ESL command failed: {reply or headers}")

    # ── public API ─────────────────────────────────────────────────────────

    def recv_event(self) -> tuple[dict[str, str], bytes]:
        """Block until one event arrives. Returns (headers, body_bytes).

        `body_bytes` is empty unless the event carried a `Content-Length` body.
        For fswtch PCM events the body is the base64-encoded PCM string.
        """
        headers = self._read_headers()
        body = b""
        n = headers.get("Content-Length")
        if n is not None:
            n = int(n)
            while len(self._buf) < n:
                chunk = self.sock.recv(4096)
                if not chunk:
                    raise ESLError("ESL socket closed mid-body")
                self._buf += chunk
            body = self._buf[:n]
            self._buf = self._buf[n:]
        return headers, body

    def send_event(
        self,
        subclass: str,
        headers: dict[str, str],
        body: bytes | str = b"",
    ) -> dict[str, str]:
        """Fire a CUSTOM event. `body` (if given) is sent verbatim as the event body.

        For fswtch PCM, pass the **base64-encoded PCM string** as `body` (the fswtch
        side decodes it). A `Content-Length` header is set to the body byte length so
        FreeSWITCH frames it unambiguously.
        """
        if isinstance(body, str):
            body = body.encode()
        lines = [b"sendevent CUSTOM", f"Event-Subclass: {subclass}".encode()]
        for k, v in headers.items():
            lines.append(f"{k}: {v}".encode())
        if body:
            lines.append(f"Content-Length: {len(body)}".encode())
        self._send(b"\n".join(lines) + b"\n\n" + body)
        return self._read_headers()  # command-reply

    def send_pcm(
        self,
        subclass: str,
        target_uuid: str,
        pcm_i16: bytes,
        sample_rate: int = 8000,
        channels: int = 1,
    ) -> dict[str, str]:
        """Convenience: base64-encode raw S16LE PCM and fire a CUSTOM event."""
        body = base64.b64encode(pcm_i16)
        return self.send_event(
            subclass,
            {
                "Target-UUID": target_uuid,
                "Sample-Rate": str(sample_rate),
                "Channels": str(channels),
                "Bits-Per-Sample": "16",
                "Sample-Format": "S16LE",
            },
            body,
        )

    def close(self) -> None:
        try:
            self.sock.close()
        except OSError:
            pass
