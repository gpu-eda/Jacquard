#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = []
# ///
"""Capture an OpenOCD `remote_bitbang` byte stream to a file.

Accepts one TCP connection from an OpenOCD client configured with the
`remote_bitbang` adapter driver, then records every byte the client sends
to the output file in raw form. For `R` (TDO-read) commands it replies
with `'0'` so OpenOCD doesn't block on reads — the recorded stream's
`R` bytes are informational only (the JTAG replay model in
`src/sim/models/jtag.rs` ignores them in stage 1; stage 2's TCP-listener
mode will route them back to the connecting OpenOCD).

Use this when you want to record a synthetic / well-known JTAG sequence
(e.g. an IDCODE scan or `scan_chain` invocation) to use as a regression
fixture in Jacquard's cosim path. To capture against a *real* target
instead, point OpenOCD at the real `remote_bitbang` server and add a
small tee — out of scope for this script.

Discussion #77 for context.

Usage:
    # 1. Start the capture server in one terminal:
    python3 scripts/capture_bitbang.py --output stream.bin --port 9999

    # 2. Point OpenOCD at it from another terminal:
    openocd \\
        -c "adapter driver remote_bitbang" \\
        -c "remote_bitbang host localhost" \\
        -c "remote_bitbang port 9999" \\
        -f your_target.cfg

    # 3. Run whatever JTAG commands you want recorded
    # (e.g. `scan_chain` at the OpenOCD prompt, or one-shot
    # `-c "init; scan_chain; exit"`). The script logs every byte
    # sent until OpenOCD disconnects.
"""

import argparse
import socket
import sys
from pathlib import Path


def serve(host: str, port: int, output: Path, verbose: bool) -> None:
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind((host, port))
    listener.listen(1)
    print(f"capture_bitbang: listening on {host}:{port} -> {output}", file=sys.stderr)

    conn, addr = listener.accept()
    print(f"capture_bitbang: client connected from {addr}", file=sys.stderr)

    # Counters surfaced when the client disconnects.
    counts: dict[str, int] = {
        "set": 0,    # '0'..='7'
        "trst": 0,   # 'r'/'s'/'t'/'u'
        "read": 0,   # 'R'
        "blink": 0,  # 'B'/'b'
        "quit": 0,   # 'Q'
        "other": 0,
    }
    total_bytes = 0
    try:
        with output.open("wb") as fout:
            while True:
                chunk = conn.recv(4096)
                if not chunk:
                    break
                fout.write(chunk)
                if verbose:
                    fout.flush()
                total_bytes += len(chunk)
                # Reply to TDO reads so OpenOCD doesn't block. Send a
                # zero (ASCII '0') for every 'R' encountered.
                reads = chunk.count(b"R")
                if reads:
                    conn.sendall(b"0" * reads)
                # Update counters for the post-disconnect summary.
                for b in chunk:
                    if 0x30 <= b <= 0x37:        # '0'..'7'
                        counts["set"] += 1
                    elif b in (0x72, 0x73, 0x74, 0x75):  # 'r''s''t''u'
                        counts["trst"] += 1
                    elif b == 0x52:              # 'R'
                        counts["read"] += 1
                    elif b in (0x42, 0x62):      # 'B''b'
                        counts["blink"] += 1
                    elif b == 0x51:              # 'Q'
                        counts["quit"] += 1
                    else:
                        counts["other"] += 1
                if b"Q" in chunk:
                    # OpenOCD sends 'Q' on shutdown; flush + exit.
                    break
    finally:
        conn.close()
        listener.close()

    print(
        f"capture_bitbang: client disconnected after {total_bytes} bytes "
        f"(set={counts['set']}, trst={counts['trst']}, read={counts['read']}, "
        f"blink={counts['blink']}, quit={counts['quit']}, "
        f"other={counts['other']})",
        file=sys.stderr,
    )
    print(f"capture_bitbang: stream written to {output}", file=sys.stderr)


def main() -> None:
    doc = __doc__ or ""
    parser = argparse.ArgumentParser(description=doc.split("\n")[0])
    parser.add_argument(
        "--output",
        type=Path,
        required=True,
        help="Path to write the recorded byte stream to (raw bytes, no framing).",
    )
    parser.add_argument(
        "--host",
        default="127.0.0.1",
        help="Bind host (default: 127.0.0.1; use 0.0.0.0 for remote clients).",
    )
    parser.add_argument(
        "--port",
        type=int,
        default=9999,
        help="Bind port (default: 9999; OpenOCD's remote_bitbang default).",
    )
    parser.add_argument(
        "--verbose",
        action="store_true",
        help="Flush output after every received chunk for live tailing.",
    )
    args = parser.parse_args()
    serve(args.host, args.port, args.output, args.verbose)


if __name__ == "__main__":
    main()
