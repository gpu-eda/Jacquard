#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Pass criterion for the X-propagation demo (#95), sim and cosim.

Reads the output VCD produced by `jacquard {sim,cosim} --xprop` on the
`xprop_demo` design and asserts the two diagnostic outputs behave as the
4-state semantics require:

  * `q_unreset` — bit 0 of an *unreset* counter (`count <= count + 1`).
    At power-up its bits are unknown and `X + 1 = X`, so it must read `x`
    for the entire run and never resolve to 0/1.

  * `q_reset`   — a flop reset by `rst_n`, then toggling. It may be `x`
    before reset is released, but must resolve to a known 0/1 afterwards
    (its final recorded value must be known).

In `two-state` mode (a run *without* `--xprop`) the same design must show
NO `x` anywhere — this guards against the doubled-state path leaking into
the default two-state simulation.

This is the end-to-end guard that was missing when the seed-template bug
(uninitialised DFF Q read as known 0) shipped: `--xprop` was silently
two-state for any sequential design and no test noticed. See ADR-0016.

Usage:
    check.py <output.vcd> {xprop|two-state}
"""
import sys

SIGNALS = ("q_unreset", "q_reset")


def parse_vcd(path):
    """Return {signal_name: {"values": set(str), "last": str|None}}.

    Tracks every value each scalar signal of interest takes, plus its
    final value. VCD scalar changes are `<val><id>` with no separator
    (e.g. `x!`, `0"`); `val` is one of 0/1/x/z.
    """
    id_to_name = {}
    for_name = {}
    with open(path) as f:
        lines = f.read().splitlines()

    # $var wire 1 <id> <name> $end
    for ln in lines:
        s = ln.strip()
        if s.startswith("$var"):
            parts = s.split()
            # $var <type> <width> <id> <name> ... $end
            if len(parts) >= 5:
                vid, name = parts[3], parts[4]
                if name in SIGNALS:
                    id_to_name[vid] = name
                    for_name[name] = {"values": set(), "last": None}

    in_defs = True
    for ln in lines:
        s = ln.strip()
        if not s:
            continue
        if in_defs:
            if s.startswith("$enddefinitions"):
                in_defs = False
            continue
        if s[0] in "$#":  # directive or timestamp
            continue
        # scalar value change: first char is the value, rest is the id
        val, vid = s[0], s[1:]
        if val in "01xzXZ" and vid in id_to_name:
            name = id_to_name[vid]
            v = val.lower()
            for_name[name]["values"].add(v)
            for_name[name]["last"] = v
    return for_name


def main() -> int:
    if len(sys.argv) != 3 or sys.argv[2] not in ("xprop", "two-state"):
        print(f"usage: {sys.argv[0]} <output.vcd> {{xprop|two-state}}", file=sys.stderr)
        return 2
    path, mode = sys.argv[1], sys.argv[2]

    sig = parse_vcd(path)
    missing = [n for n in SIGNALS if n not in sig]
    if missing:
        print(f"FAIL: signals not found in {path}: {missing}", file=sys.stderr)
        return 1

    for name in SIGNALS:
        vals = "".join(sorted(sig[name]["values"])) or "(none)"
        print(f"  {name}: values={{{vals}}} last={sig[name]['last']}")

    ok = True

    if mode == "xprop":
        # q_unreset must be X for the whole run and never resolve.
        uv = sig["q_unreset"]["values"]
        if uv != {"x"}:
            ok = False
            print(
                f"FAIL: q_unreset should be x for the entire run, saw {sorted(uv)} "
                "(uninitialised counter — X+1=X must stay X)",
                file=sys.stderr,
            )
        # q_reset must resolve to a known value by the end.
        rl = sig["q_reset"]["last"]
        if rl not in ("0", "1"):
            ok = False
            print(
                f"FAIL: q_reset should resolve to known 0/1 after reset, last={rl}",
                file=sys.stderr,
            )
    else:  # two-state
        for name in SIGNALS:
            if "x" in sig[name]["values"] or "z" in sig[name]["values"]:
                ok = False
                print(
                    f"FAIL: {name} has x/z in two-state run (no --xprop): "
                    f"{sorted(sig[name]['values'])}",
                    file=sys.stderr,
                )

    if ok:
        print(f"PASS: xprop_demo {mode} output matches 4-state expectations")
        return 0
    return 1


if __name__ == "__main__":
    sys.exit(main())
