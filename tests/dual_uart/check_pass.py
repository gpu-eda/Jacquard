#!/usr/bin/env python3
"""Verify dual-UART cosim produced the expected byte sequences."""

import json
import sys


def main():
    if len(sys.argv) < 2:
        print(f"Usage: {sys.argv[0]} <output_events.json>", file=sys.stderr)
        sys.exit(1)

    with open(sys.argv[1]) as f:
        data = json.load(f)

    events = data.get("events", data) if isinstance(data, dict) else data

    console_bytes = [e["payload"] for e in events if e["peripheral"] == "console"]
    debug_bytes = [e["payload"] for e in events if e["peripheral"] == "debug"]

    expected_console = [0x48, 0x69]  # "Hi"
    expected_debug = [0x4F, 0x4B]  # "OK"

    ok = True
    if console_bytes != expected_console:
        print(
            f"FAIL: console expected {expected_console}, got {console_bytes}",
            file=sys.stderr,
        )
        ok = False
    else:
        print(f"console: {bytes(console_bytes)!r} OK")

    if debug_bytes != expected_debug:
        print(
            f"FAIL: debug expected {expected_debug}, got {debug_bytes}",
            file=sys.stderr,
        )
        ok = False
    else:
        print(f"debug:   {bytes(debug_bytes)!r} OK")

    if not ok:
        sys.exit(1)
    print("PASS: both UART channels decoded correctly")


if __name__ == "__main__":
    main()
