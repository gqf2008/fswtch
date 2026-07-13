"""Minimal FreeSWITCH ESL raw-socket server for **outbound** mode (zero dependency).

FreeSWITCH's `socket` dialplan application opens a TCP connection TO us (one per
call). Each connection is pre-trusted (no `auth`), carries the call's channel data
on connect, and lets us drive the call (`park`, `bridge`, `hangup`, …) + exchange
`fswtch::vad` / `fswtch::uplink_pcm` / `fswtch::downlink_pcm` events.

Usage::

    server = ESLServer("127.0.0.1", 8084)
    while True:
        session = server.accept()           # blocks until FS dials in
        ch = session.read_channel_data()    # Unique-ID, Variable_*, …
        session.send_cmd("park")
        session.send_cmd("bridge fswtch_vad_bot/1000")
        headers, body = session.recv_event()
"""

from __future__ import annotations

import socket
import base64
import urllib.parse


class ESLError(RuntimeError):
    pass


# ── low-level helpers shared by ESLSession ─────────────────────────────────


def _read_line(sock: socket.socket, buf: list[bytes]) -> bytes:
    """Read one `\\n`-terminated line from `sock`, buffering leftovers in `buf`."""
    while b"\n" not in b"".join(buf):
        chunk = sock.recv(4096)
        if not chunk:
            raise ESLError("ESL socket closed")
        buf.append(chunk)
    joined = b"".join(buf)
    line, rest = joined.split(b"\n", 1)
    buf.clear()
    buf.append(rest)
    return line


def _read_headers(sock: socket.socket, buf: list[bytes]) -> dict[str, str]:
    """Read a `Key: Value\\n` header block terminated by a blank line."""
    headers: dict[str, str] = {}
    while True:
        line = _read_line(sock, buf)
        if line == b"":
            break  # blank line terminates the block
        if b": " in line:
            k, v = line.split(b": ", 1)
            headers[k.decode(errors="replace")] = v.decode(errors="replace")
    return headers


# ── per-call session ───────────────────────────────────────────────────────


class ESLSession:
    """One outbound ESL connection = one call.

    FreeSWITCH opens this connection when the dialplan hits
    `<action application="socket" data="host:port full"/>`. No auth is needed —
    the connection is pre-trusted. Read the channel data with
    [`read_channel_data`](#brain.esl.ESLSession.read_channel_data), drive the call
    with [`send_cmd`](#brain.esl.ESLSession.send_cmd), and exchange events via
    [`recv_event`](#brain.esl.ESLSession.recv_event) /
    [`send_event`](#brain.esl.ESLSession.send_event).
    """

    def __init__(self, sock: socket.socket, addr: tuple[str, int]):
        self.sock = sock
        self.addr = addr
        self._buf: list[bytes] = []

    # ── low-level ───────────────────────────────────────────────────────────

    def _send(self, data: bytes) -> None:
        self.sock.sendall(data)

    # ── channel data (received once on connect) ─────────────────────────────

    def read_channel_data(self) -> dict[str, str]:
        """Send the ``connect`` command and read the ``CHANNEL_DATA`` event FS
        responds with.

        FreeSWITCH's outbound socket protocol requires the brain to send
        ``connect\\n\\n`` after the TCP connection is established; FS then sends
        a ``CHANNEL_DATA`` event as raw ``Key: Value\\n`` lines (URL-encoded,
        terminated by a blank line — no ``Content-Type``/``Content-Length``
        framing). Contains the A-leg's ``Unique-ID``, ``Channel-Name``, and all
        channel variables (``Variable_<name>`` headers).
        """
        self._send(b"connect\n\n")
        raw = _read_headers(self.sock, self._buf)
        # ESL URL-encodes event header values (e.g. `::` → `%3A%3A`).
        return {k: urllib.parse.unquote(v) for k, v in raw.items()}

    # ── call control ────────────────────────────────────────────────────────

    def send_cmd(self, cmd: str) -> dict[str, str]:
        """Send an ESL command (e.g. ``"event plain ALL"``, ``"park"``) and read
        the ``command/reply``. Returns the reply headers (including
        ``Reply-Text``, which starts with ``+OK`` on success).
        """
        self._send(f"{cmd}\n\n".encode())
        return _read_headers(self.sock, self._buf)

    def send_app(self, app: str, arg: str = "") -> dict[str, str]:
        """Execute a dialplan application on the channel via ``sendmsg``.

        Use this for applications that aren't direct ESL commands (e.g.
        ``bridge``, ``playback``). ``park`` and ``event`` work as direct
        [`send_cmd`](#brain.esl.ESLSession.send_cmd) calls, but dialplan
        applications need ``sendmsg`` framing in outbound socket mode.
        """
        lines = ["sendmsg", "call-command: execute", f"execute-app-name: {app}"]
        if arg:
            lines.append(f"execute-app-arg: {arg}")
        return self.send_cmd("\n".join(lines))

    # ── events ──────────────────────────────────────────────────────────────

    def recv_event(self) -> tuple[dict[str, str], bytes]:
        """Block until one event arrives. Returns ``(headers, body_bytes)``.

        ``body_bytes`` is empty unless the event carried a ``Content-Length``
        body (e.g. the base64 PCM string for ``fswtch::uplink_pcm``).
        """
        top = _read_headers(self.sock, self._buf)
        n = top.get("Content-Length")
        if n is not None:
            n = int(n)
            while len(b"".join(self._buf)) < n:
                chunk = self.sock.recv(4096)
                if not chunk:
                    raise ESLError("ESL socket closed mid-body")
                self._buf.append(chunk)
            joined = b"".join(self._buf)
            data = joined[:n]
            self._buf.clear()
            self._buf.append(joined[n:])
        else:
            data = b""

        # ESL `event plain` wraps the event as: top-level `Content-Length`/
        # `Content-Type`, then the Content-Length body is the EVENT serialization
        # — event headers as `Key: Value\n` lines, optionally followed by a blank
        # line (`\n\n`) + the event's own body (e.g. the base64 PCM). Parse those
        # out so callers see real event headers.
        event_headers: dict[str, str] = {}
        event_body = b""
        if b"\n\n" in data:
            header_part, event_body = data.split(b"\n\n", 1)
        else:
            header_part = data
        for line in header_part.split(b"\n"):
            if b": " in line:
                k, v = line.split(b": ", 1)
                # ESL `event plain` URL-encodes header values (e.g. `::` → `%3A%3A`).
                event_headers[k.decode(errors="replace")] = urllib.parse.unquote(
                    v.decode(errors="replace")
                )
        return event_headers, event_body

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
        return _read_headers(self.sock, self._buf)  # command-reply

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


# ── TCP listener ───────────────────────────────────────────────────────────


class ESLServer:
    """Listens for outbound ESL connections from FreeSWITCH.

    ``accept`` blocks until the dialplan's ``socket`` application dials in, then
    returns a per-call [`ESLSession`].
    """

    def __init__(self, host: str = "127.0.0.1", port: int = 8084):
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.sock.bind((host, port))
        self.sock.listen(8)

    def accept(self) -> ESLSession:
        sock, addr = self.sock.accept()
        sock.settimeout(None)  # blocking reads after accept
        return ESLSession(sock, addr)
