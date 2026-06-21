#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""
bitbang_client.py — a minimal `remote_bitbang` *client* for the
`jacquard cosim --jtag-server <PORT>` loopback gate (V1 in
docs/plans/jtag-debug-server.md).

It plays the role OpenOCD plays: connect to the JTAG server, stream a
recorded `remote_bitbang` byte sequence, and read back one byte for
every `R` (read-TDO) command so the server never blocks on its reply.

This pins live-vs-replay equivalence: feeding the SAME `bitbang.rec`
the `--jtag-replay` gate uses must drive the design to the same
`data0_obs == 0xCAFEBABE`, proving the live socket → TDO read-back →
drive path matches the deterministic replay path end-to-end, with zero
external tooling (Python only).

Usage:
    bitbang_client.py <host> <port> <bitbang.rec> [--connect-timeout S]

Exit codes:
    0  whole stream sent (and every `R` answered)
    1  connection or I/O error
"""
import argparse
import socket
import sys
import time
from pathlib import Path


def connect(host: str, port: int, deadline: float) -> socket.socket:
    """Retry-connect until `deadline` — the server binds then blocks on
    accept() during cosim setup, which can lag the client's launch."""
    last_err: Exception | None = None
    while time.monotonic() < deadline:
        try:
            s = socket.create_connection((host, port), timeout=5.0)
            s.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
            return s
        except OSError as e:  # noqa: PERF203 — retry loop
            last_err = e
            time.sleep(0.25)
    raise SystemExit(f"bitbang_client: could not connect to {host}:{port}: {last_err}")


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("host")
    ap.add_argument("port", type=int)
    ap.add_argument("stream", type=Path)
    ap.add_argument(
        "--connect-timeout",
        type=float,
        default=120.0,
        help="seconds to keep retrying the initial connect (default 120)",
    )
    args = ap.parse_args(argv[1:])

    data = args.stream.read_bytes()
    sock = connect(args.host, args.port, time.monotonic() + args.connect_timeout)
    sock.settimeout(600.0)  # generous: the server paces consumption per edge

    reads = 0
    try:
        # Byte-at-a-time, reading the reply immediately after each `R`.
        # That guarantees the server's 1-byte TDO replies never back up
        # (no deadlock) regardless of how it paces byte consumption.
        for b in data:
            sock.sendall(bytes([b]))
            if b == ord("R"):
                resp = sock.recv(1)
                if not resp:
                    print(
                        f"bitbang_client: server closed after {reads} R replies",
                        file=sys.stderr,
                    )
                    return 1
                reads += 1
    except OSError as e:
        print(f"bitbang_client: socket error after {reads} R replies: {e}",
              file=sys.stderr)
        return 1
    finally:
        # Half-close so the server sees EOF and stops blocking on reads.
        try:
            sock.shutdown(socket.SHUT_WR)
        except OSError:
            pass
        sock.close()

    print(f"bitbang_client: sent {len(data)} bytes, answered {reads} TDO reads")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
