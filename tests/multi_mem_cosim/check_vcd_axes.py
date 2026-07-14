# SPDX-License-Identifier: Apache-2.0
#
# Assert the output VCD and the stimulus VCD share a time axis (#195).
#
# Both are written from one cosim run off the same `edge_tick`, so they are read
# together — `clk` is a primary input and lives in the stimulus, while the design
# state it clocks lives in the output. If the two axes disagree, overlaying them
# silently lies: #195 made a correct ÷2 derived clock read as ÷4 that way.
#
# The output VCD emits only changes, so it is a *subset* of the stimulus grid,
# never a superset and never longer. That asymmetry is what this checks:
#
#   1. every output timestamp lands on the stimulus grid (the scheduler's gcd),
#   2. the output never runs past the end of the stimulus.
#
# Usage: python3 check_vcd_axes.py <output.vcd> <stimulus.vcd>
import sys


def timestamps(path):
    """Return the ordered list of `#<n>` timestamps in a VCD."""
    out = []
    with open(path) as f:
        for raw in f:
            line = raw.strip()
            if line.startswith("#") and line[1:].isdigit():
                out.append(int(line[1:]))
    return out


def grid_of(stamps):
    """Infer the tick grid: the smallest gap between consecutive stamps."""
    gaps = {b - a for a, b in zip(stamps, stamps[1:]) if b > a}
    return min(gaps) if gaps else 0


def main():
    if len(sys.argv) != 3:
        print("usage: check_vcd_axes.py <output.vcd> <stimulus.vcd>", file=sys.stderr)
        return 2
    out_ts = timestamps(sys.argv[1])
    stim_ts = timestamps(sys.argv[2])

    if not out_ts or not stim_ts:
        print(
            f"FAIL: empty VCD (output={len(out_ts)} stimulus={len(stim_ts)} stamps)",
            file=sys.stderr,
        )
        return 1

    grid = grid_of(stim_ts)
    if grid == 0:
        print("FAIL: could not infer a stimulus grid", file=sys.stderr)
        return 1

    off = [t for t in out_ts if t % grid != 0]
    if off:
        print(
            f"FAIL: {len(off)} output timestamp(s) off the {grid}ps stimulus grid: "
            f"{off[:5]}",
            file=sys.stderr,
        )
        return 1

    if out_ts[-1] > stim_ts[-1]:
        ratio = out_ts[-1] / stim_ts[-1]
        print(
            f"FAIL: output VCD runs past the stimulus: {out_ts[-1]} > {stim_ts[-1]} "
            f"({ratio:.2f}x). The two axes disagree (#195).",
            file=sys.stderr,
        )
        return 1

    print(
        f"PASS: output axis agrees with stimulus (grid {grid}ps, "
        f"output ends {out_ts[-1]} <= stimulus {stim_ts[-1]})"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
