#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Assert that GPU backends produce byte-identical simulation output.

The CUDA, HIP, and Metal kernels implement the *same* boolean-processor
algorithm (`simulate_block_v1`); the output VCD is written by shared Rust
code, so a `jacquard sim` of one design+stimulus must yield an identical VCD
on every backend. This is the cross-backend equivalence guard: if the three
diverge, a kernel ported to one backend has drifted from the others — the
exact failure mode that hand-maintained twin kernels invite.

Volatile header lines (`$date`, `$version`, `$comment` blocks) are stripped
before comparison; everything else (timescale, `$var` declarations, scope,
and every value-change line) must match exactly.

Usage:
    compare_backend_vcds.py --label <name> <vcd> <vcd> [<vcd> ...]

Exit code 0 if all inputs are identical after normalisation, 1 otherwise
(with a unified diff of the first mismatch on stderr).
"""
import argparse
import difflib
import sys
from pathlib import Path

# VCD declaration blocks whose contents are run-dependent (wall-clock date,
# tool version string, free-form comments) and so must be ignored.
VOLATILE_BLOCKS = ("$date", "$version", "$comment")


def normalise(path: Path) -> list[str]:
    """Return the VCD's lines with volatile `$date/$version/$comment` blocks
    removed. A block runs from its opening keyword to the next `$end`."""
    lines = path.read_text().splitlines()
    out: list[str] = []
    skip_until_end = False
    for ln in lines:
        stripped = ln.strip()
        if skip_until_end:
            if stripped.endswith("$end"):
                skip_until_end = False
            continue
        if stripped.startswith(VOLATILE_BLOCKS):
            # Single-line form (`$version ... $end`) or multi-line.
            if not stripped.endswith("$end"):
                skip_until_end = True
            continue
        out.append(ln)
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--label", required=True, help="comparison name for logs")
    ap.add_argument("vcds", nargs="+", help="VCD files to compare (>=2)")
    args = ap.parse_args()

    if len(args.vcds) < 2:
        print("need at least two VCDs to compare", file=sys.stderr)
        return 2

    paths = [Path(p) for p in args.vcds]
    missing = [p for p in paths if not p.is_file()]
    if missing:
        print(f"FAIL [{args.label}]: missing inputs: {missing}", file=sys.stderr)
        return 1

    ref_path, *rest = paths
    ref = normalise(ref_path)
    ok = True
    for other_path in rest:
        other = normalise(other_path)
        if other != ref:
            ok = False
            print(
                f"FAIL [{args.label}]: {other_path} differs from {ref_path}",
                file=sys.stderr,
            )
            diff = difflib.unified_diff(
                ref, other,
                fromfile=str(ref_path), tofile=str(other_path),
                lineterm="",
            )
            # Cap the dump so a wholesale divergence doesn't flood the log.
            for i, line in enumerate(diff):
                if i >= 60:
                    print("  ... (diff truncated)", file=sys.stderr)
                    break
                print(line, file=sys.stderr)

    if ok:
        print(
            f"PASS [{args.label}]: {len(paths)} backends produce identical VCDs "
            f"({ref_path.name} == " + ", ".join(p.name for p in rest) + ")"
        )
        return 0
    return 1


if __name__ == "__main__":
    sys.exit(main())
