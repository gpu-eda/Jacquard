#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""
check_pass.py — assert that a cosim run of tests/jtag_minimal/
produced the expected `data0_obs = 0xCAFEBABE`.

Reads the cosim's `--timing-vcd` output, reconstructs the 32-bit
`data0_obs` value from its 32 individual signals, and exits with:
  0  on PASS (data0_obs ended at 0xCAFEBABE)
  1  on FAIL with a diagnostic table

Used by CI to gate the jtag_minimal regression — when `data0_obs`
doesn't reach the magic value, the diagnostic table prints
dmactive_obs / haltreq_obs / per-bit data0_obs values so the
failure cause is visible without re-running locally.
"""
import re
import sys
from pathlib import Path

EXPECTED = 0xCAFEBABE


def parse_vcd(vcd_path: Path) -> dict[str, str]:
    """Return {signal_name: final_value} for every $var wire 1 in the VCD."""
    txt = vcd_path.read_text()
    sym2name = {m.group(1): m.group(2) for m in re.finditer(r"\$var wire \d+ (\S+) (\S+)", txt)}
    last = {}
    for line in txt.splitlines():
        m = re.match(r"^([01])(\S)$", line)
        if m:
            last[m.group(2)] = m.group(1)
    return {name: last.get(sym, "?") for sym, name in sym2name.items()}


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print(f"usage: {argv[0]} <cosim_trace.vcd>", file=sys.stderr)
        return 2
    vcd_path = Path(argv[1])
    if not vcd_path.exists():
        print(f"ERROR: VCD not found: {vcd_path}", file=sys.stderr)
        return 2
    finals = parse_vcd(vcd_path)

    bits = [finals.get(f"data0_obs[{i}]", "?") for i in range(32)]
    bits_msb_lsb = "".join(reversed(bits))
    try:
        data0_v = int(bits_msb_lsb, 2)
    except ValueError:
        data0_v = None

    dmactive = finals.get("dmactive_obs", "?")
    haltreq = finals.get("haltreq_obs", "?")

    if data0_v == EXPECTED:
        print(f"PASS: data0_obs = 0x{data0_v:08x}  "
              f"(dmactive_obs={dmactive}  haltreq_obs={haltreq})")
        return 0

    print("FAIL: data0_obs final value", file=sys.stderr)
    print(f"  expected: 0x{EXPECTED:08x}", file=sys.stderr)
    if data0_v is None:
        print(f"  observed: contains X bits — final pattern (LSB..MSB) "
              f"= {''.join(bits)}", file=sys.stderr)
    else:
        print(f"  observed: 0x{data0_v:08x}", file=sys.stderr)
    print(f"  dmactive_obs final: {dmactive}", file=sys.stderr)
    print(f"  haltreq_obs  final: {haltreq}", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
