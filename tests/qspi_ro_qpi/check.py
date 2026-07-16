#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Semantic pass criterion for the read-only QPI flash cosim test (#222).

Parses the output VCD produced by `jacquard cosim --output-vcd` and asserts
that the DUT quad-read the byte preloaded in the *read-only* flash's firmware
image — i.e. a `writable = false` instance still honoured `enter_qpi_cmd` and
served a QPI `0xEB` read. Specifically:

  * `rdata` settles to 0xA5 (fw/ro_flash.bin holds 0xA5 at address 0x000001), and
  * `match` (rdata == 0xA5) is asserted by end of run.

The DUT also quad-writes 0xA5 to that address, but the write is redundant here:
the firmware already holds 0xA5, and a read-only instance must ignore writes.
So the read can only succeed by serving preloaded content over QPI.

Usage: check.py <output.vcd>
"""
import re
import sys


def parse_vcd_final(path):
    """Return {signal_name: final_value_str} for scalar + vector signals."""
    id2name = {}
    for line in open(path):
        m = re.match(r"\$var \w+ \d+ (\S+) (\S+)", line)
        if m:
            id2name[m.group(1)] = m.group(2)
    last = {}
    for line in open(path):
        line = line.rstrip("\n")
        if not line:
            continue
        if line[0] in "01xz":
            val, ident = line[0], line[1:]
            if ident in id2name:
                last[id2name[ident]] = val
        elif line[0] == "b":
            m = re.match(r"b([01xz]+) (\S+)", line)
            if m and m.group(2) in id2name:
                last[id2name[m.group(2)]] = m.group(1)
    return last


def main() -> int:
    if len(sys.argv) != 2:
        print(f"usage: {sys.argv[0]} <output.vcd>", file=sys.stderr)
        return 2
    last = parse_vcd_final(sys.argv[1])

    # Reconstruct rdata[7:0] from its per-bit scalar nets.
    rdata = 0
    for i in range(8):
        if last.get(f"rdata[{i}]", "0") == "1":
            rdata |= 1 << i
    match = last.get("match", "x")
    done = last.get("done", "x")

    print(f"Final: rdata=0x{rdata:02X} match={match} done={done}")

    ok = (rdata == 0xA5) and (match == "1") and (done == "1")
    if ok:
        print("PASS: read-only QPI flash served preloaded 0xA5 over a 0xEB quad read")
        return 0
    print(
        "FAIL: expected rdata=0xA5, match=1, done=1 "
        f"(got rdata=0x{rdata:02X}, match={match}, done={done}) — "
        "a read-only instance never entered QPI, so the 4-lane 0xEB was "
        "misparsed as single-lane and MISO stayed 0 (gpu-eda/Jacquard#222)",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
