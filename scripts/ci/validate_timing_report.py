#!/usr/bin/env python3
"""Validate the shape of a `jacquard sim --timing-report` JSON file.

Shared by the Metal / CUDA / HIP CI jobs (#104) so the schema check lives in
one place. Checks structure only — not specific violation counts (the
cross-backend equivalence job compares actual values). Exits non-zero on any
structural problem.

Usage: validate_timing_report.py <timing_report.json>
"""

import json
import sys


def validate(path: str) -> list[str]:
    with open(path) as f:
        report = json.load(f)

    errors: list[str] = []

    # Top-level keys
    for key in ("schema_version", "metadata", "stats", "violations", "per_word", "worst_slack"):
        if key not in report:
            errors.append(f"missing top-level key: {key}")

    # schema_version is semver-like
    sv = report.get("schema_version", "")
    if len(sv.split(".")) != 3:
        errors.append(f"schema_version not semver: {sv}")

    # metadata fields
    meta = report.get("metadata", {})
    for key in ("design", "clock_period_ps", "cycles_run", "jacquard_version"):
        if key not in meta:
            errors.append(f"missing metadata.{key}")
    if meta.get("cycles_run", 0) < 1:
        errors.append(f"cycles_run too low: {meta.get('cycles_run')}")

    # stats fields
    stats = report.get("stats", {})
    for key in ("setup_violations", "hold_violations"):
        if key not in stats:
            errors.append(f"missing stats.{key}")

    # violations is an array
    if not isinstance(report.get("violations"), list):
        errors.append("violations is not an array")

    # worst_slack has setup and hold arrays
    ws = report.get("worst_slack", {})
    for key in ("setup", "hold"):
        if not isinstance(ws.get(key), list):
            errors.append(f"worst_slack.{key} is not an array")

    if not errors:
        print(
            f"Timing report valid: schema {sv}, {meta.get('cycles_run')} cycles, "
            f"{stats.get('setup_violations', 0)} setup / "
            f"{stats.get('hold_violations', 0)} hold violations"
        )
    return errors


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: validate_timing_report.py <timing_report.json>", file=sys.stderr)
        return 2
    errors = validate(sys.argv[1])
    for e in errors:
        print(f"FAIL: {e}", file=sys.stderr)
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
