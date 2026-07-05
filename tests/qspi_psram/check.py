#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Semantic pass criterion for the QSPI PSRAM (RAM-mode flash) cosim test.

Parses the output VCD produced by `jacquard cosim --output-vcd` and asserts
that the DUT quad-read back the exact byte it quad-wrote — i.e. the writable
QSPI-PSRAM peripheral round-tripped a write→read. Specifically:

  * `match` (rdata == 0xA5) is asserted by end of run, and
  * `rdata` settles to 0xA5 (the byte written to address 0x000001).

This is the content check; the byte-identical cross-backend gate is the diff
against tests/qspi_psram/expected/qspi_psram.vcd in cosim_cpu_check.sh.

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
        print("PASS: QSPI PSRAM quad-write 0xA5 -> quad-read-back 0xA5 (match asserted)")
        return 0
    print(
        "FAIL: expected rdata=0xA5, match=1, done=1 "
        f"(got rdata=0x{rdata:02X}, match={match}, done={done})",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
