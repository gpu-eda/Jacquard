# Handoff — Architecture docs migration (Part 2: the ADR→Decisions rename + move)

**Updated:** 2026-08-10
**Branch:** `docs/architecture-migration-part2` (off `main`; worktree `~/Code/jacquard-migration-part2`)
**PR:** [gpu-eda/Jacquard#232](https://github.com/gpu-eda/Jacquard/pull/232) — open, CI green.
**Status of the redesign:** Part 1 (foundation) is **merged to `main`**. Part 2 —
both **PR A (the mechanical move)** and **PR B (the six-area reference layer)** — is
**DONE and pushed on #232**. Only optional polish and handoff resolution remain
(see [Remaining](#remaining--optional-polish--resolution)).

## PR A — DONE (committed and pushed on #232)

Two commits on `docs/architecture-migration-part2`:
- `f9792e1d` — the move + all references + `SUMMARY.md`
- `8b9b4df5` — the `decisions/README.md` immutable-log convention rewrite

What landed:
- `git mv docs/adr/*.md → docs/architecture/decisions/` (numbers kept as stable
  IDs); **0003 → `decisions/archive/`**; `docs/spikes/* → decisions/spikes/`.
- The full cross-reference surface rewritten, each path resolved relative to its
  source file (a resolve-relative migration script, not blind sed — the two
  scripts are in this session's scratchpad if a re-run is needed). Watch item:
  the trap here is that a file which *itself* moved deeper also needs its links to
  *non-moved* targets re-depthed (`../plans/` → `../../plans/`); a second pass
  fixed 33 such links. The link checker is the completeness gate for this.
- `ADR NNNN` → `Decision NNNN` across the metric surface (docs, CLAUDE.md,
  `.github/`, `crates/`); stale `docs/adr/` **paths** fixed everywhere incl.
  `src/`, `tests/`.
- `SUMMARY.md` → the Architecture tree; `decisions/README.md` → the immutable /
  past-tense convention (drops the "ADR" term + the dated-Amendment mechanism,
  points current-state at the reference layer, documents `archive/`).

**Verification (all green at HEAD):**
```sh
cd ~/Code/jacquard-migration-part2 && mdbook build && python3 docs/scripts/check_doc_links.py
# build exit 0; "All rendered-page links resolve to rendered pages."
grep -rnE "docs/adr/|ADR [0-9]{4}" docs/ CLAUDE.md .github/ crates/ | grep -v architecture-docs-migration-handoff | wc -l
# 0  (this handoff still holds old refs on purpose; it is git rm'd at resolution)
```

## Settled since the original handoff (apply in PR B)

- **0016 (X-propagation) → Simulation engine** (confirmed; was the unconfirmed default).
- **0003 → `decisions/archive/`** (confirmed; done in PR A).

## PR B — DONE on this branch (PR #232)

- **All six area reference docs** written and wired (Timing correctness, Simulation
  engine, Cosim runtime, RTL on-ramp, PDK enablement, Distribution). Move-vs-new was
  decided per area: `simulation-engine` absorbed the old `simulation-architecture.md`;
  the other new docs link to their how-to/surface/contract guides rather than
  duplicating them. `architecture/README.md` map marks all six written. A **fourth
  built-in PDK, IHP SG13G2**, surfaced during drafting and is now recorded.
- **Two-way linking done**: each decision carries a `**Current architecture:**`
  forward-link to its area doc (all 21 active; 0003 archived is skipped). 0019 is
  cross-cutting (PDK + timing).
- **Bare-term "ADR" → "decision" sweep** done for the reader-facing docs
  (`docs/*.md`, `docs/plans/*.md`); metric stays 0.
- **Plans nav** split into active "Implementation Plans" and "Completed plans".
- **House prose style** added (`docs/prose-style.md`, wired into CLAUDE.md +
  development.md); global `~/.claude/PROSE_STYLE.md` gained the interjection rule.

## Remaining — optional polish + resolution

1. **Aggressive per-decision-body reframe** (deliberately NOT done): stripping
   current-state prose from the decision bodies and retiring each `(amended …)`
   status. Skipped as risky on cited, near-immutable records for modest gain now
   that the area docs carry the "what". Revisit only if the redundancy grates.
2. **Inline-code path-depth gap** (PR-A residue): backtick prose refs like
   `` `../timing-correctness.md` `` inside moved decision/spike/archive files should
   be `` `../../` ``. They're prose, not links, so nothing 404s. Not auto-fixed
   because `timing-correctness.md` now exists at two depths (area doc + contract), so
   a naive "does it resolve" fix can land on the wrong same-named file. Fix by hand.
3. **`accepted-rtl.md` "Providing yosys.wasm"** wants a fuller refresh against the
   current `src/synth.rs` resolution order (the one clearly-false line is already
   fixed; `rtl-onramp.md` is now the authoritative source).
4. **Resolve this handoff**: fold anything durable into the migrated docs, then
   `git rm` this file once PR #232 merges (merge queue, rebase-only).

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
5. **Six-area taxonomy**: Timing correctness / Simulation engine / Cosim runtime /
   RTL on-ramp / PDK enablement / Distribution. Placements confirmed and shipped:
   cell metadata (0010, 0011) and cell-model IR (0019) → PDK enablement; **0016
   (X-propagation) → Simulation engine** (confirmed by Rob); 0019 is cross-cutting
   and also appears under Timing correctness.
6. **0003 → `decisions/archive/`** (confirmed and done in PR A). A superseded decision
   moves to `archive/` with `Status: Superseded`.

(The original seven Part-2 steps are all done — see the PR A / PR B DONE sections
above. Only the optional-polish tail in Remaining is left.)

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

- [`docs/architecture/decisions/spikes/architecture-doc-redesign.md`](../architecture/decisions/spikes/architecture-doc-redesign.md) — THE spec (taxonomy, tree, naming, field survey)
- [`docs/architecture/README.md`](../architecture/README.md) — the by-area map (all six rows now written)
- [`docs/architecture/cosim-runtime.md`](../architecture/cosim-runtime.md) and [`simulation-engine.md`](../architecture/simulation-engine.md) — the pattern the other four area docs follow
- [`docs/architecture/decisions/README.md`](../architecture/decisions/README.md) — the immutable-log convention + three-kinds model
- [`docs/prose-style.md`](../prose-style.md) — the house writing style (no mid-sentence interjections)
- [`docs/architecture-diagrams.md`](../architecture-diagrams.md) — the D2 how-to
- [`docs/handoff-discipline.md`](../handoff-discipline.md) — how to resolve this handoff (fold in, then delete)

---

**Resume in a new session with:**
```
cd ~/Code/jacquard-migration-part2   # this worktree/branch carries the current handoff
/resume_handoff docs/handoffs/architecture-docs-migration-handoff.md
```

The migration is functionally complete on #232. A resuming session's job is the
optional-polish tail above (or none of it), then — once #232 merges via the queue —
fold anything durable into the docs and `git rm` this handoff per
`docs/handoff-discipline.md`.
