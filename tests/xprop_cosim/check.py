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

Modes:
    xprop        sim/cosim with --xprop: q_unreset stays X, q_reset resolves.
    xprop-cosim  cosim only — also checks the undriven-input outputs (#95
                 phase 3): q_undriven_comb stays X (pure-comb of an undriven
                 input) and q_undriven_reg becomes X after reset (reset flop
                 that samples the undriven input). sim leaves these known
                 because sim inputs all come from the VCD.
    two-state    a run WITHOUT --xprop: no X/Z on any present signal.
    reg-init     cosim with --xprop AND a `reg_init` deposit on the unreset
                 counter (issue #108): q_unreset must now be KNOWN 0/1 for the
                 whole run — the $deposit cleared the power-up X that mode
                 `xprop` requires to persist. The inverse assertion of `xprop`,
                 proving register value-injection works end to end.

Usage:
    check.py <output.vcd> {xprop|xprop-cosim|two-state|reg-init}
"""
import sys

# Power-up DFF X-sources, present in both sim and cosim.
CORE = ("q_unreset", "q_reset")
# Undriven-input outputs, only meaningful in cosim (#95 phase 3).
UNDRIVEN = ("q_undriven_comb", "q_undriven_reg")
SIGNALS = CORE + UNDRIVEN
MODES = ("xprop", "xprop-cosim", "two-state", "reg-init")


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
    if len(sys.argv) != 3 or sys.argv[2] not in MODES:
        print(f"usage: {sys.argv[0]} <output.vcd> {{{'|'.join(MODES)}}}", file=sys.stderr)
        return 2
    path, mode = sys.argv[1], sys.argv[2]

    sig = parse_vcd(path)
    # Which signals this mode requires present. CORE always; undriven only in
    # the cosim mode (sim has no undriven-input concept).
    required = CORE + (UNDRIVEN if mode == "xprop-cosim" else ())
    missing = [n for n in required if n not in sig]
    if missing:
        print(f"FAIL: signals not found in {path}: {missing}", file=sys.stderr)
        return 1

    for name in (n for n in SIGNALS if n in sig):
        vals = "".join(sorted(sig[name]["values"])) or "(none)"
        print(f"  {name}: values={{{vals}}} last={sig[name]['last']}")

    ok = True

    def fail(msg):
        nonlocal ok
        ok = False
        print(f"FAIL: {msg}", file=sys.stderr)

    if mode == "two-state":
        for name in (n for n in SIGNALS if n in sig):
            bad = sig[name]["values"] & {"x", "z"}
            if bad:
                fail(f"{name} has {sorted(bad)} in a two-state run (no --xprop)")
    elif mode == "reg-init":
        # q_unreset: the unreset counter was seeded via `reg_init` ($deposit),
        # so its power-up X is cleared — it must be a KNOWN 0/1 for the whole
        # run (the LSB toggles each cycle from a definite seed). This is the
        # exact inverse of the `xprop` mode and proves value-injection works.
        bad = sig["q_unreset"]["values"] & {"x", "z"}
        if bad:
            fail(
                f"q_unreset still has {sorted(bad)} after a reg_init deposit "
                "(value-injection did not clear the power-up X)"
            )
        if sig["q_unreset"]["last"] not in ("0", "1"):
            fail(f"q_unreset should end known, last={sig['q_unreset']['last']}")
        # q_reset is unaffected by reg_init; it must still resolve.
        if sig["q_reset"]["last"] not in ("0", "1"):
            fail(f"q_reset should resolve to known 0/1, last={sig['q_reset']['last']}")
    else:  # xprop or xprop-cosim
        # q_unreset: uninitialised counter, X+1=X, must be X for the whole run.
        if sig["q_unreset"]["values"] != {"x"}:
            fail(
                f"q_unreset should be x for the entire run, saw "
                f"{sorted(sig['q_unreset']['values'])} (X+1=X must stay X)"
            )
        # q_reset: reset flop, must resolve to a known value by the end.
        if sig["q_reset"]["last"] not in ("0", "1"):
            fail(f"q_reset should resolve to known 0/1, last={sig['q_reset']['last']}")

        if mode == "xprop-cosim":
            # Pure-comb function of the undriven input: X for the whole run.
            if sig["q_undriven_comb"]["values"] != {"x"}:
                fail(
                    f"q_undriven_comb should be x for the entire run, saw "
                    f"{sorted(sig['q_undriven_comb']['values'])} "
                    "(undriven primary input → X through comb logic)"
                )
            # Reset flop sampling the undriven input: known under reset, then X.
            if sig["q_undriven_reg"]["last"] != "x":
                fail(
                    f"q_undriven_reg should become x after reset (samples an "
                    f"undriven input), last={sig['q_undriven_reg']['last']}"
                )

    if ok:
        print(f"PASS: xprop_demo {mode} output matches 4-state expectations")
        return 0
    return 1


if __name__ == "__main__":
    sys.exit(main())
