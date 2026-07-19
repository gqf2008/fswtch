#!/usr/bin/env python3
"""End-to-end verification for FreeSWITCH's `fswtch_unicast` endpoint module.

Proves the module's media path is implemented correctly, using only the
Python standard library. The script originates a call

    fswtch_unicast/127.0.0.1:<port> &echo

and then, over the single raw-PCM UDP socket, checks:

  Phase 1  silence framing — before we send anything, the module emits
           well-formed 320-byte L16 frames of pure silence (the echo app
           loops back the silence it reads from the endpoint).
  Phase 2  round-trip order — uniquely marked frames sent to the module
           come back bit-exact and in FIFO order (UDP recv -> read_frame
           -> echo app -> write_frame -> UDP send). Frames may be dropped
           by the module's bounded channel under CPU pacing, but never
           reordered or corrupted.
  Phase 3  source filter — frames sent from a *different* source port are
           never echoed back: the module only accepts packets from the
           negotiated peer address.

Prerequisites: FreeSWITCH running with `fswtch_unicast` loaded, and
`fs_cli` on PATH (or pass --fs-cli). No third-party packages.

Exit code 0 = all phases passed; 1 = a check failed.
"""

import argparse
import socket
import struct
import subprocess
import sys
import time

SAMPLES_PER_FRAME = 160          # 20 ms at 8 kHz mono
FRAME_BYTES = SAMPLES_PER_FRAME * 2
MARKER_FILL = 1000               # payload sample value for legit marked frames
INTRUDER_FILL = -2000            # payload sample value for intruder frames
INTRUDER_BASE = 30000            # marker base that cannot collide with legit ids


def fs_cli(fs_cli_path, cmd):
    """Run an fs_cli command, returning its stdout."""
    out = subprocess.run(
        [fs_cli_path, "-x", cmd], capture_output=True, text=True, timeout=30
    )
    return out.stdout.strip()


def make_marked_frame(marker, fill):
    """A frame whose first two samples carry `marker`, rest are `fill`."""
    return struct.pack("<hh", marker, marker) + struct.pack("<h", fill) * (
        SAMPLES_PER_FRAME - 2
    )


def frame_marker(payload, fill):
    """Return the marker id if `payload` is a marked frame with `fill`, else None."""
    if len(payload) != FRAME_BYTES:
        return None
    samples = struct.unpack(f"<{SAMPLES_PER_FRAME}h", payload)
    if samples[0] != samples[1]:
        return None
    if any(s != fill for s in samples[2:]):
        return None
    return samples[0]


class Peer:
    def __init__(self, bind_port):
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.bind(("127.0.0.1", bind_port))
        self.sock.settimeout(0.2)
        self.module_addr = None

    def recv_frames(self, duration):
        """Collect (payload, addr, recv_time) for `duration` seconds."""
        frames = []
        deadline = time.monotonic() + duration
        while time.monotonic() < deadline:
            try:
                data, addr = self.sock.recvfrom(4096)
            except socket.timeout:
                continue
            frames.append((data, addr, time.monotonic()))
        return frames

    def drain_until_first(self, timeout):
        """Wait for the module's first packet; remember its source address."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                data, addr = self.sock.recvfrom(4096)
            except socket.timeout:
                continue
            self.module_addr = addr
            return data
        return None

    def send_frame(self, payload):
        self.sock.sendto(payload, self.module_addr)


def phase_silence(peer):
    """Phase 1: module emits well-formed 320 B frames of silence."""
    frames = peer.recv_frames(1.0)
    if not frames:
        return False, "no packets from module within 1s of call setup"
    sizes = {len(p) for p, _, _ in frames}
    if sizes != {FRAME_BYTES}:
        return False, f"unexpected packet sizes {sizes} (want {FRAME_BYTES})"
    total = zero = 0
    for payload, _, _ in frames:
        for (s,) in struct.iter_unpack("<h", payload):
            total += 1
            zero += s == 0
    if zero / total < 0.99:
        return False, f"expected silence, got {zero / total:.1%} zero samples"
    return True, f"{len(frames)} frames, all {FRAME_BYTES} B, {zero / total:.1%} zeros"


def phase_roundtrip(peer, count=150, interval=0.015, window=4.0):
    """Phase 2: marked frames come back bit-exact and in FIFO order."""
    sent = 0
    received = []  # markers in receive order
    deadline = time.monotonic() + window
    next_send = time.monotonic()
    while time.monotonic() < deadline:
        now = time.monotonic()
        if sent < count and now >= next_send:
            peer.send_frame(make_marked_frame(sent, MARKER_FILL))
            sent += 1
            next_send = now + interval
        try:
            data, _ = peer.sock.recvfrom(4096)
        except socket.timeout:
            continue
        m = frame_marker(data, MARKER_FILL)
        if m is not None:
            received.append(m)
    if len(received) < max(10, count // 3):
        return (
            False,
            f"only {len(received)}/{count} marked frames echoed back "
            "(channel starvation?)",
        )
    if received != sorted(received):
        return False, f"marked frames reordered: {received[:20]}..."
    if len(set(received)) != len(received):
        return False, "duplicate marked frames (payload corruption?)"
    return (
        True,
        f"{len(received)}/{count} marked frames echoed back bit-exact, strictly in order",
    )


def phase_source_filter(peer, count=50, window=2.0):
    """Phase 3: packets from a foreign source port are never accepted."""
    intruder = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    intruder.bind(("127.0.0.1", 0))  # different source port than the peer
    try:
        for i in range(count):
            intruder.sendto(
                make_marked_frame(INTRUDER_BASE + i, INTRUDER_FILL), peer.module_addr
            )
        # Meanwhile keep the legit stream alive and watch for intruder data.
        leaked = 0
        legit = 0
        deadline = time.monotonic() + window
        next_send = time.monotonic()
        marker = 10000
        while time.monotonic() < deadline:
            now = time.monotonic()
            if now >= next_send:
                peer.send_frame(make_marked_frame(marker, MARKER_FILL))
                marker += 1
                next_send = now + 0.02
            try:
                data, _ = peer.sock.recvfrom(4096)
            except socket.timeout:
                continue
            if frame_marker(data, INTRUDER_FILL) is not None:
                leaked += 1
            elif frame_marker(data, MARKER_FILL) is not None:
                legit += 1
        if leaked:
            return False, f"{leaked} intruder frames accepted (source filter broken)"
        if legit == 0:
            return False, "legit stream stalled during filter test"
        return True, f"0/{count} intruder frames accepted; legit stream alive ({legit})"
    finally:
        intruder.close()


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--port", type=int, default=5000, help="local UDP port to bind (default 5000)")
    ap.add_argument("--host", default="127.0.0.1", help="FreeSWITCH host for the dialstring (default 127.0.0.1)")
    ap.add_argument("--fs-cli", default="fs_cli", help="path to fs_cli (default: fs_cli)")
    ap.add_argument(
        "--no-originate",
        action="store_true",
        help="do not place the call; just wait for a manually originated one",
    )
    args = ap.parse_args()

    peer = Peer(args.port)
    uuid = None

    if not args.no_originate:
        out = fs_cli(args.fs_cli, f"originate fswtch_unicast/{args.host}:{args.port} &echo")
        print(f"originate: {out}")
        if "+OK" not in out:
            print("FAIL: originate did not succeed (is fswtch_unicast loaded?)")
            return 1
        uuid = out.split()[-1]

    try:
        first = peer.drain_until_first(timeout=15)
        if first is None:
            print("FAIL: no media from module within 15s")
            return 1
        print(f"module media address: {peer.module_addr[0]}:{peer.module_addr[1]}")

        checks = [
            ("1. silence framing", phase_silence(peer)),
            ("2. round-trip order", phase_roundtrip(peer)),
            ("3. source filter", phase_source_filter(peer)),
        ]
        ok = True
        for name, (passed, detail) in checks:
            print(f"{'PASS' if passed else 'FAIL'}  {name}: {detail}")
            ok = ok and passed
        return 0 if ok else 1
    finally:
        if uuid:
            fs_cli(args.fs_cli, f"uuid_kill {uuid}")


if __name__ == "__main__":
    sys.exit(main())
