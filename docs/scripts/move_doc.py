#!/usr/bin/env python3
"""Move docs to another directory and repoint every reference to them.

Usage::

    python3 docs/scripts/move_doc.py [--dry-run] SRC [SRC ...] DEST_DIR

Typical uses are archiving a shipped plan
(``docs/plans/<name>.md docs/plans/archive/``) and a superseded decision
(``docs/architecture/decisions/<NNNN-title>.md docs/architecture/decisions/archive/``).
For each ``SRC`` the script:

  1. ``git mv``\\ s it into ``DEST_DIR``;
  2. rewrites every relative Markdown link in the repo's tracked ``.md`` files
     that resolves to a moved file, recomputing the path from the linking file;
  3. re-depths the relative links *inside* each moved file, so its links to
     other docs keep resolving from the new directory;
  4. rewrites repo-root paths (``docs/plans/<name>.md``) in every tracked text
     file, which covers source comments, ``Cargo.toml`` descriptions and CI.

It does not regroup ``SUMMARY.md`` or index tables beyond repointing their
links; ``check_doc_links.py`` fails if an archived plan is left outside
"Completed plans". Links inside fenced code blocks are rewritten like any
other. Mentions the script cannot safely rewrite (a relative path in inline
code, for instance) are listed at the end for a manual pass.

Running it twice is harmless: a source already in ``DEST_DIR`` is skipped.
Stdlib only.
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent.parent

# Inline links ](target) and reference definitions `[label]: target`.
LINK_RE = re.compile(r"(\]\(|^\s*\[[^\]]+\]:\s*)([^)\s]+)", re.MULTILINE)
EXTERNAL = ("http://", "https://", "mailto:", "#", "/")


class MoveError(Exception):
    """A source that cannot be moved."""


def git(*args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=REPO_ROOT, check=True, capture_output=True, text=True
    ).stdout


def plan_moves(srcs: list[str], dest_dir: Path) -> dict[Path, Path]:
    """Map each source to its destination, skipping ones already moved."""
    moves: dict[Path, Path] = {}
    for arg in srcs:
        src = Path(arg).resolve()
        dst = dest_dir / src.name
        if not src.exists() and dst.exists():
            print(f"skip: {arg} is already in {dest_dir.relative_to(REPO_ROOT)}")
        elif not src.is_file():
            raise MoveError(f"no such file: {arg}")
        elif dst.exists():
            raise MoveError(f"destination exists: {dst.relative_to(REPO_ROOT)}")
        else:
            moves[src] = dst
    return moves


def rewrite_links(
    text: str, old_dir: Path, new_dir: Path, moves: dict[Path, Path]
) -> str:
    """Rewrite relative links in a file that lived in ``old_dir`` and now lives
    in ``new_dir``. A link is touched only if its target moved or the file did."""

    def sub(m: re.Match[str]) -> str:
        raw = m.group(2)
        if raw.startswith(EXTERNAL):
            return m.group(0)
        target, sep, anchor = raw.partition("#")
        resolved = Path(os.path.normpath(old_dir / target))
        new_target = moves.get(resolved, resolved)
        if new_target == resolved and new_dir == old_dir:
            return m.group(0)
        rel = Path(os.path.relpath(new_target, new_dir)).as_posix()
        return m.group(1) + rel + sep + anchor

    return LINK_RE.sub(sub, text)


def main() -> int:
    ap = argparse.ArgumentParser(
        description="Move docs to another directory and repoint every reference."
    )
    ap.add_argument("srcs", nargs="+", metavar="SRC", help="file to move")
    ap.add_argument("dest_dir", metavar="DEST_DIR", help="directory to move into")
    ap.add_argument("--dry-run", action="store_true", help="report, change nothing")
    args = ap.parse_args()

    dest_dir = Path(args.dest_dir).resolve()
    try:
        moves = plan_moves(args.srcs, dest_dir)
    except MoveError as e:
        ap.error(str(e))
    if not moves:
        return 0

    root_paths = {
        src.relative_to(REPO_ROOT).as_posix(): dst.relative_to(REPO_ROOT).as_posix()
        for src, dst in moves.items()
    }
    # Any file that refers to a moved file names it, so a fixed-string grep for
    # the file names finds every candidate without reading the whole repo.
    grep_args = [arg for src in moves for arg in ("-e", src.name)]
    candidates = {
        REPO_ROOT / p for p in git("grep", "-lzIF", *grep_args).split("\0") if p
    } | set(moves)
    leftover_re = re.compile(
        "|".join(
            rf"\b{re.escape(src.parent.name)}/{re.escape(src.name)}\b" for src in moves
        )
    )

    edits: dict[Path, str] = {}
    leftovers: list[str] = []
    for path in sorted(candidates):
        text = path.read_text()
        final = moves.get(path, path)
        new = text
        if path.suffix == ".md":
            new = rewrite_links(new, path.parent, final.parent, moves)
        for old_path, new_path in root_paths.items():
            new = new.replace(old_path, new_path)
        if new != text:
            edits[final] = new
        for lineno, line in enumerate(new.splitlines(), 1):
            if leftover_re.search(line):
                leftovers.append(
                    f"  {final.relative_to(REPO_ROOT)}:{lineno}: {line.strip()}"
                )

    for src, dst in moves.items():
        print(f"move: {src.relative_to(REPO_ROOT)} -> {dst.relative_to(REPO_ROOT)}")
    for final in sorted(set(edits) - set(moves.values())):
        print(f"edit: {final.relative_to(REPO_ROOT)}")

    if not args.dry_run:
        dest_dir.mkdir(parents=True, exist_ok=True)
        for src, dst in moves.items():
            git("mv", str(src), str(dst))
        for final, text in edits.items():
            final.write_text(text)

    if leftovers:
        print(f"\n{len(leftovers)} mention(s) left for a manual check:")
        print("\n".join(leftovers))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
