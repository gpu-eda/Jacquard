#!/usr/bin/env python3
"""Validate intra-doc Markdown links against the *rendered* mdBook page set.

mdBook only renders pages listed in ``docs/SUMMARY.md``. A link from a rendered
page to a ``.md`` file that exists on disk but is *not* in SUMMARY produces a
404 on the published site (e.g. ``project-scope.html``) — the kind of breakage a
plain file-existence check misses.

This script:
  1. parses ``docs/SUMMARY.md`` to build the set of rendered pages;
  2. for every rendered page, checks each ``](target.md#anchor)`` link resolves
     to a file that both exists on disk *and* is itself a rendered page;
  3. prints (non-fatally) the inventory of ``.md`` files not in SUMMARY so doc
     drift stays visible.

Exit status is non-zero iff any rendered page has a broken/404-ing link.
Stdlib only — runs identically locally (``python3 scripts/check_doc_links.py``)
and in CI, independent of the mdBook binary version.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DOCS = REPO_ROOT / "docs"
SUMMARY = DOCS / "SUMMARY.md"

# Markdown inline links: ](target). Capture the target up to ) or whitespace.
LINK_RE = re.compile(r"\]\(([^)\s]+)\)")
# SUMMARY list entries: - [Title](path.md)
SUMMARY_LINK_RE = re.compile(r"\]\(([^)\s]+\.md)\)")

# Directories whose pages are deliberately out-of-book working memory.
IGNORE_ORPHAN_DIRS = {"handoffs"}


def rendered_pages() -> set[str]:
    """Set of page paths (posix, relative to docs/) that mdBook renders."""
    pages: set[str] = set()
    for m in SUMMARY_LINK_RE.finditer(SUMMARY.read_text()):
        target = m.group(1).split("#", 1)[0]
        if target:
            pages.add((DOCS / target).resolve().relative_to(DOCS).as_posix())
    return pages


def main() -> int:
    pages = rendered_pages()
    errors: list[str] = []

    for page in sorted(pages):
        page_path = DOCS / page
        if not page_path.exists():
            errors.append(f"SUMMARY.md lists a page that does not exist: {page}")
            continue
        for m in LINK_RE.finditer(page_path.read_text()):
            raw = m.group(1)
            target = raw.split("#", 1)[0]
            if not target or target.startswith(("http://", "https://", "mailto:")):
                continue
            if not target.endswith(".md"):
                continue  # assets / api dir / bare anchors handled elsewhere
            resolved = (page_path.parent / target).resolve()
            rel = (
                resolved.relative_to(DOCS).as_posix()
                if DOCS in resolved.parents
                else None
            )
            if not resolved.exists():
                errors.append(f"{page}: broken link -> {raw} (no such file)")
            elif rel is None or rel not in pages:
                errors.append(
                    f"{page}: link -> {raw} targets a page not in SUMMARY.md "
                    f"(would 404 on the published site)"
                )

    # Non-fatal drift inventory.
    on_disk = {
        p.relative_to(DOCS).as_posix()
        for p in DOCS.rglob("*.md")
        if p.name != "SUMMARY.md"
        and not (set(p.relative_to(DOCS).parts) & IGNORE_ORPHAN_DIRS)
    }
    orphans = sorted(on_disk - pages)
    if orphans:
        print(f"note: {len(orphans)} doc(s) not in SUMMARY.md (not rendered):")
        for o in orphans:
            print(f"  - {o}")

    if errors:
        print(
            f"\n{len(errors)} broken/404-ing doc link(s):", file=sys.stderr
        )
        for e in errors:
            print(f"  {e}", file=sys.stderr)
        return 1
    print("\nAll rendered-page links resolve to rendered pages.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
