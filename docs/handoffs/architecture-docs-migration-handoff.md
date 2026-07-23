# Handoff — Architecture docs migration (Part 2: the ADR→Decisions rename + move)

**Updated:** 2026-07-23
**Branch:** `docs/architecture-migration-part2` (off `main`; worktree `~/Code/jacquard-migration-part2`)
**Status of the redesign:** Part 1 (the foundation) is **merged to `main`**. This
handoff is Part 2 — the actual migration — which is **not started**.

## Goal & next-up

Execute the migration that turns today's `docs/adr/` into the top-level
**Architecture** structure the spike designed and Part 1 laid the foundation for.
The design is settled and ratified (below); this is mechanical-but-wide execution
that must not be done as a partial rename.

**Next session should start with step 1 (the cross-reference audit)** — do not
`git mv` anything before the full reference surface is enumerated.

**Verification command (run continuously during the migration):**
```sh
cd ~/Code/jacquard-migration-part2 && mdbook build && python3 docs/scripts/check_doc_links.py
# Expect: build succeeds; "All rendered-page links resolve to rendered pages."
grep -rnE "docs/adr/|ADR [0-9]{4}" docs/ CLAUDE.md .github/ crates/ | wc -l
# This count starts high (~the reference surface) and must reach 0 as refs move
# to docs/architecture/decisions/ and the "ADR" term is dropped for "Decision".
```

## What is DONE and on `main` (Part 1 — do not redo)

Merged 2026-07-21 via the merge queue (PRs #227 foundation, #228 ADR 0022, #229
CI sccache, #184 the signal-stream feature — all landed):

- **`docs/adr/README.md`** carries the **three-kinds-of-claim** model (why /
  current-what / decided-but-unbuilt age differently). It does **not** carry the
  living-ADR edit-in-place convention (that was deliberately left off — see the
  immutable ratification below).
- **`docs/spikes/architecture-doc-redesign.md`** — the full design: the split, the
  Diátaxis spine, the six-area taxonomy, naming, the doc tree, and the field survey.
  This is the spec for Part 2. Read it first.
- **Worked examples**: `docs/architecture/README.md` (the by-area map) and
  `docs/architecture/cosim-runtime.md` (one area's "what" doc with a live D2
  diagram). **Mirror `cosim-runtime.md` when writing the other five area docs.**
- **D2 diagram system** (built, CI-green, warm-cache-validated): `mdbook-d2` (elk)
  in `book.toml` + the docs CI job, theme-adaptive via `theme/d2-diagrams.css`,
  how-to in `docs/architecture-diagrams.md`. Reuse it; don't revisit it.
- **`docs/scripts/check_doc_links.py`** skips fenced code blocks (so illustrative
  markdown — e.g. a future `SUMMARY.md` sample — doesn't false-404).
- **ADR 0022** exists (`docs/adr/0022-flow-controlled-io.md`, Proposed).
- **CI**: mdbook is 0.5.2 (mdbook-d2 0.3.8 needs it); sccache + `CARGO_BUILD_JOBS=nproc/2`
  are wired into the compile jobs (`.github/actions/setup-sccache`), warm C/C++
  cache-hit rate measured at 100%.

## Ratified decisions (source of truth — apply these in Part 2)

1. **Immutable / past-tense decision log.** This *reverses* the living-ADR rule from
   commit `0704c2f0`: once the reference layer carries current state, a decision is a
   point-in-time record; a changed decision is a **new superseding record**, not an
   in-place rewrite. Does **not** revive dated "Amendment" blocks (both models reject
   those). Part 2 rewrites the decision docs to this model and updates the convention
   prose in the decisions README accordingly.
2. **Drop the term "ADR" → "Decisions".** The section, the directory
   (`docs/architecture/decisions/`), and the nav all use "Decision"/"Decisions".
   Rendered titles come from `SUMMARY.md` link text + the `# H1`.
3. **Structure by directory, not filename prefix.** Keep `NNNN-kebab-title.md`
   filenames (NOT `dec-NNNN-*`/`arch-N-*`). The directory (`architecture/` vs
   `architecture/decisions/`) encodes the class. The 4-digit number is a stable ID
   and must survive so every "ADR 0022"/`0022-*` reference still resolves conceptually.
4. **Everything nests under `docs/architecture/`**: the area docs, `decisions/`, and
   `decisions/spikes/` (spikes are a decision's evidence → nested under decisions;
   sibling `architecture/spikes/` is the rejected alternative).
5. **Six-area taxonomy** (full ADR→area table in the spike's "Taxonomy" section):
   Timing correctness / Simulation engine / Cosim runtime / RTL on-ramp / PDK
   enablement / Distribution. Two placements settled: **cell metadata (0010, 0011) →
   PDK enablement**; **X-propagation (0016) → Simulation engine** *(this was my default,
   never explicitly confirmed by Rob — reconfirm before relying on it)*.
6. **0003 is NOT archived.** An earlier plan moved it to `adr/archive/`; that was
   reverted. Part 2 decides how the new design records superseded decisions (0003 is
   the one Superseded decision) — likely a `decisions/` status, not the old archival.

## Part 2 steps (large; do in reviewable slices, on this branch)

1. **Audit the cross-reference surface FIRST.** Enumerate every `docs/adr/` path and
   "ADR NNNN" prose mention across `docs/`, `CLAUDE.md`, `SUMMARY.md`, `docs/plans/`,
   `.github/` (CI + `check_doc_links.py` assumptions), `crates/` (e.g. the opensta
   integration test references ADRs), and commit-message conventions. A scout/grep
   pass producing the full list is the right first step. **Never partial-rename.**
2. **Create the tree** and move: `git mv docs/adr/ docs/architecture/decisions/`,
   `git mv docs/spikes/ docs/architecture/decisions/spikes/`. Keep `NNNN-*.md` names.
   Rename `adr/README.md` → `decisions/README.md`.
3. **Write the five remaining `arch-*` area docs**, mirroring `cosim-runtime.md`:
   present-tense current state, each section forward-linking to its decision(s) by
   number, an `## Implementation status` for the decided-but-unbuilt, a D2 diagram
   only where a shape is genuinely 2-D.
4. **Reframe the decision docs** to immutable/past-tense: strip current-state prose
   that now lives in the arch docs, past-tense the rationale, add a forward-link to
   the arch doc, decide 0003's superseded representation.
5. **Rewrite `SUMMARY.md`** to the tree in the spike (Architecture: map → area docs →
   Decisions → spikes). Note `SUMMARY.md` currently has BOTH the video-tap spike and
   the redesign spike (kept both when #184 + #227 merged) — preserve/relocate both.
6. **Fix every reference** from step 1's audit (paths + prose "ADR NNNN" → "Decision
   NNNN" or the new path), and **update `CLAUDE.md`** (points at `docs/adr/` in several
   places) and any CI path assumptions.
7. **Resolve this handoff**: fold anything durable into the migrated docs, then
   `git rm` this file (per `docs/handoff-discipline.md`). Open the PR.

## Critical context / gotchas

- **Never do a partial rename.** The reference surface is wide; the link check catches
  rendered-page links but NOT prose "ADR 0022" mentions or `CLAUDE.md`/CI. Audit-then-move.
- **Decision numbers are stable IDs** — whatever the filename/section becomes, the
  4-digit number survives so historical references stay meaningful.
- **Diagram styling is verified via headless Chrome, not D2 PNGs** (a PNG shows the
  baked palette, not the themed result). Recipe in `docs/architecture-diagrams.md`.
- **`main` is protected** (merge queue, linear history) — land Part 2 via a PR to the
  queue, not a direct push. Merges are **rebase-only**.
- **CI compile jobs OOM-guard**: `mt-kahypar` C++ builds at `-O3`; the `nproc/2` cap +
  sccache handle it. If a job flakes with bare "compilation terminated", it's a
  transient runner OOM — `gh run rerun --failed`, don't chase it as a real error.

## References

- [`docs/spikes/architecture-doc-redesign.md`](../spikes/architecture-doc-redesign.md) — THE spec (taxonomy, tree, naming, field survey)
- [`docs/architecture/cosim-runtime.md`](../architecture/cosim-runtime.md) — the worked "what" doc to mirror for the other five areas
- [`docs/architecture/README.md`](../architecture/README.md) — the by-area map
- [`docs/adr/README.md`](../adr/README.md) — three-kinds model lives here; becomes `decisions/README.md`
- [`docs/architecture-diagrams.md`](../architecture-diagrams.md) — the D2 how-to
- [`docs/handoff-discipline.md`](../handoff-discipline.md) — how to resolve this handoff (fold in, then delete)

---

**Resume in a new session with:**
```
git checkout docs/architecture-migration-part2   # this branch carries the current handoff
/resume_handoff docs/handoffs/architecture-docs-migration-handoff.md
```
