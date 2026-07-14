# SPDX-License-Identifier: Apache-2.0
#
# Content assertion for the derived (÷2) clock fixture (gpu-eda/Jacquard#185).
#
# The DUT is an on-die ÷2 toggle-FF divider (`clkdiv = clk ÷ 2`) driving an
# 8-bit counter clocked by the DERIVED clock. This checks the semantic the
# byte-diff golden can't state directly:
#
#   1. `clkdiv` is a clean toggle (alternating 0/1 rising/falling edges).
#   2. the counter advances by exactly +1 (mod 256) once per `clkdiv` rising
#      edge — i.e. it is clocked by the derived clock, not the master clock.
#   3. there is exactly one counter increment per `clkdiv` period (the "half
#      rate" property: the counter ticks once per two master-clock cycles).
#
# Usage: python3 check.py <output.vcd>
import sys


def parse_vcd(path):
    """Return (symbol->name map, list of (time, symbol, value) changes)."""
    sym2name = {}
    changes = []
    t = 0
    in_dump = False
    with open(path) as f:
        for raw in f:
            line = raw.strip()
            if line.startswith("$var"):
                # $var wire 1 ! clkdiv $end
                parts = line.split()
                sym, name = parts[3], parts[4]
                sym2name[sym] = name
            elif line == "$dumpvars":
                in_dump = True
            elif line.startswith("#"):
                t = int(line[1:])
            elif in_dump and line and line[0] in "01":
                # scalar change: <value><symbol>
                val, sym = line[0], line[1:]
                changes.append((t, sym, val))
    return sym2name, changes


def main():
    if len(sys.argv) != 2:
        print("usage: check.py <output.vcd>", file=sys.stderr)
        return 2
    sym2name, changes = parse_vcd(sys.argv[1])
    name2sym = {v: k for k, v in sym2name.items()}

    if "clkdiv" not in name2sym:
        print("FAIL: no `clkdiv` signal in VCD", file=sys.stderr)
        return 1
    clkdiv_sym = name2sym["clkdiv"]
    # count bits count[0..7]
    bit_syms = {}
    for b in range(8):
        nm = f"count[{b}]"
        if nm not in name2sym:
            print(f"FAIL: missing counter bit {nm}", file=sys.stderr)
            return 1
        bit_syms[name2sym[nm]] = b

    clkdiv = 0
    count = 0
    count_at_rising = []  # counter value sampled just before each clkdiv 0->1
    clkdiv_edges = 0
    prev_clkdiv = 0

    # Replay changes in order. VCD groups changes by timestamp; the counter
    # update lands a little after the clkdiv edge, so sampling the count at the
    # instant clkdiv rises captures the value from the PREVIOUS period.
    for _t, sym, val in changes:
        v = int(val)
        if sym == clkdiv_sym:
            if prev_clkdiv == 0 and v == 1:
                clkdiv_edges += 1
                count_at_rising.append(count)
            prev_clkdiv = v
            clkdiv = v
        elif sym in bit_syms:
            b = bit_syms[sym]
            if v:
                count |= 1 << b
            else:
                count &= ~(1 << b)

    _ = clkdiv  # settle final value (unused beyond the loop)

    if clkdiv_edges < 3:
        print(f"FAIL: too few clkdiv rising edges ({clkdiv_edges})", file=sys.stderr)
        return 1

    # The counter value captured at each successive clkdiv rising edge must
    # increase by exactly 1 (the previous period's single increment), mod 256.
    ok = True
    for i in range(1, len(count_at_rising)):
        prev, cur = count_at_rising[i - 1], count_at_rising[i]
        if (prev + 1) & 0xFF != cur:
            print(
                f"FAIL: counter not +1/clkdiv at edge {i}: {prev} -> {cur}",
                file=sys.stderr,
            )
            ok = False
    if not ok:
        return 1

    print(
        f"PASS: {clkdiv_edges} clkdiv rising edges, counter +1 each "
        f"(sequence {count_at_rising[:8]}...)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
