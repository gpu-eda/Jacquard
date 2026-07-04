#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Pass criterion for the multi-memory cosim test (ADR 0013 plural QSPI + SRAM).

Parses the output VCD and asserts each of the three QSPI flash instances and
the two on-chip SRAMs returned its OWN distinct contents — i.e. every memory
has an independent backing store:

  f0 == 0xA1, f1 == 0xB2, f2 == 0xC3   (three flashes, distinct firmware)
  s0 == 0xA1, s1 == 0xB2               (two SRAMs, distinct values)
  all_match == 1, done == 1

Usage: check.py <output.vcd>
"""
import re
import sys

EXPECT = {"f0": 0xA1, "f1": 0xB2, "f2": 0xC3, "s0": 0xA1, "s1": 0xB2}


def parse_final(path):
    id2name = {}
    for line in open(path):
        m = re.match(r"\$var \w+ \d+ (\S+) (.+?) \$end", line)
        if m:
            id2name[m.group(1)] = m.group(2).strip()
    last = {}
    for line in open(path):
        line = line.rstrip("\n")
        if not line:
            continue
        if line[0] in "01xz":
            last[id2name.get(line[1:], line[1:])] = line[0]
        elif line[0] == "b":
            m = re.match(r"b([01xz]+) (\S+)", line)
            if m:
                last[id2name.get(m.group(2), m.group(2))] = m.group(1)
    return last


def busval(last, prefix, w=8):
    bits = "".join(last.get(f"{prefix}[{b}]", "x") for b in range(w - 1, -1, -1))
    return int(bits, 2) if set(bits) <= set("01") else None


def main():
    last = parse_final(sys.argv[1])
    ok = True
    for name, exp in EXPECT.items():
        got = busval(last, name)
        status = "OK" if got == exp else "MISMATCH"
        if got != exp:
            ok = False
        gs = f"0x{got:02X}" if got is not None else "x/undriven"
        print(f"  {name}: got {gs}, expect 0x{exp:02X}  [{status}]")
    for scalar in ("done", "all_match"):
        got = last.get(scalar, "?")
        if got != "1":
            ok = False
        print(f"  {scalar}: {got}  [{'OK' if got == '1' else 'FAIL'}]")
    if not ok:
        print("FAIL: not every memory returned its own distinct contents")
        sys.exit(1)
    print("PASS: 3 independent QSPI flashes + 2 independent SRAMs")


if __name__ == "__main__":
    main()
