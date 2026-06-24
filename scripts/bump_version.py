#!/usr/bin/env python3
"""Single source of truth for the Rust crate versions.

The first-party Rust crates ship together in one release tarball
(`jacquard` + the `opensta-to-ir` / `timing-ir` helpers it bundles), so they
carry one shared version. They are NOT a Cargo workspace (the crates build
into their own `target/` dirs, which several CI/release steps depend on — see
`docs/release-process.md`), so the version cannot be inherited via
`[workspace.package]`. This script keeps the `[package].version` fields locked
together instead, and the release workflow calls it in `--check` mode as a
verify-guard so a mistagged release fails before publishing.

`netlist-graph` (the Python companion) versions independently — it is not a
Cargo crate, so it never appears in the discovered manifest set below.

Usage:
    python3 scripts/bump_version.py 0.2.2          # set all crates to 0.2.2
    python3 scripts/bump_version.py --check 0.2.2   # assert all == 0.2.2
    python3 scripts/bump_version.py --check         # assert all agree
    python3 scripts/bump_version.py --print-version  # assert agree, print it
"""
from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

# The first-party Rust crates that share one version: the root crate plus every
# crate under `crates/`. Discovered rather than enumerated so a new first-party
# crate is enrolled automatically instead of being silently left un-bumped.
MANIFESTS = [
    REPO_ROOT / "Cargo.toml",
    *sorted((REPO_ROOT / "crates").glob("*/Cargo.toml")),
]

SEMVER = re.compile(r"^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$")
# Captures the version value inside a `version = "..."` line; group 2 is the
# value. Applied only to lines inside the `[package]` table (see _find_version),
# so inline dependency `version = "..."` fields are never matched.
VERSION_LINE = re.compile(r'(\s*version\s*=\s*")([^"]*)(")')


def _find_version(lines: list[str]) -> tuple[int, re.Match[str]]:
    """Return (line index, match) for the `[package].version` line."""
    in_package = False
    for i, line in enumerate(lines):
        stripped = line.strip()
        if stripped.startswith("[") and stripped.endswith("]"):
            in_package = stripped == "[package]"
            continue
        if in_package:
            m = VERSION_LINE.match(line)
            if m:
                return i, m
    raise ValueError("no [package].version found")


def read_version(manifest: Path) -> str:
    _, m = _find_version(manifest.read_text().splitlines(keepends=True))
    return m.group(2)


def set_version(manifest: Path, version: str) -> str:
    lines = manifest.read_text().splitlines(keepends=True)
    i, m = _find_version(lines)
    current = m.group(2)
    if current != version:
        lines[i] = lines[i][: m.start(2)] + version + lines[i][m.end(2) :]
        manifest.write_text("".join(lines))
    return current


def _agreed_version(expected: str | None) -> tuple[str | None, str]:
    """Return (version, '') if the crates agree (and match expected), else
    (None, error message)."""
    versions = {m: read_version(m) for m in MANIFESTS}
    distinct = set(versions.values())
    if len(distinct) != 1:
        detail = "\n".join(
            f"  {m.relative_to(REPO_ROOT)}: {v}" for m, v in versions.items()
        )
        return None, f"Rust crate versions disagree:\n{detail}"
    (found,) = distinct
    if expected is not None and found != expected:
        return None, f"expected all Rust crates at {expected}, found {found}"
    return found, ""


def cmd_set(version: str) -> int:
    if not SEMVER.match(version):
        print(f"error: {version!r} is not a valid semantic version", file=sys.stderr)
        return 2
    for manifest in MANIFESTS:
        old = set_version(manifest, version)
        rel = manifest.relative_to(REPO_ROOT)
        print(f"  {rel}: {'already ' + version if old == version else old + ' -> ' + version}")
    return 0


def cmd_check(expected: str | None) -> int:
    found, err = _agreed_version(expected)
    if found is None:
        print(f"error: {err}", file=sys.stderr)
        return 1
    print(f"ok: all {len(MANIFESTS)} Rust crates at {found}")
    return 0


def cmd_print_version(expected: str | None) -> int:
    found, err = _agreed_version(expected)
    if found is None:
        print(f"error: {err}", file=sys.stderr)
        return 1
    print(found)
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument(
        "--check",
        action="store_true",
        help="verify the crates agree (and equal VERSION if given) instead of writing",
    )
    mode.add_argument(
        "--print-version",
        action="store_true",
        help="like --check, but print only the agreed version to stdout",
    )
    parser.add_argument("version", nargs="?", help="target version, e.g. 0.2.2")
    args = parser.parse_args(argv)

    if args.print_version:
        return cmd_print_version(args.version)
    if args.check:
        return cmd_check(args.version)
    if not args.version:
        parser.error("a target VERSION is required unless --check is given")
    return cmd_set(args.version)


if __name__ == "__main__":
    raise SystemExit(main())
