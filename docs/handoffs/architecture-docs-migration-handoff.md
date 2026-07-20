# Handoff — Architecture docs migration (execute the redesign spike)

**Created:** 2026-07-20
**Working tree:** clean after the final commit below (diagram doc + spike structure + this handoff still to commit at handoff time)
**Branch:** `signal-stream-rebase2` → pushed to `feat/cosim-signal-stream-tap`

## Goal & next-up

**Goal of this session:** spike a redesign of the architecture docs (split
what/why/unbuilt into a reference layer + decision log, organised by area under a
top-level "Architecture") and build the D2 diagram system that supports it. Both
are done and pushed. The user has approved *executing* the redesign — this handoff
carries that execution.

**Next session should pick up:** the migration itself, but **only after the user
ratifies the two open decisions below** (§ Open follow-up 1). Do not start moving
files before those are settled — the taxonomy determines where everything lands,
and the immutable-vs-living call changes how each decision doc gets rewritten.

**Verification command:**
```sh
cd <worktree> && mdbook build && python3 docs/scripts/check_doc_links.py
# Expect: build succeeds; "All rendered-page links resolve to rendered pages."
grep -rnE "docs/adr/|ADR [0-9]{4}" docs/ CLAUDE.md | wc -l
# During migration this count drops to 0 as refs move to the new paths/names.
```

## What's decided and built (don't redo)

The full design is in [`docs/spikes/architecture-doc-redesign.md`](../spikes/architecture-doc-redesign.md).
Load-bearing decisions already made:

- **The split.** Three claim types age differently — why / current-what /
  decided-but-unbuilt — and get three artifact classes. Diátaxis
  reference-vs-explanation is the spine. The three-bucket model is already written
  into [`docs/adr/README.md`](../adr/README.md) ("Three kinds of claim, aged
  differently").
- **The doc tree.** Top-level `docs/architecture/`: a `README.md` map, `arch-N-*.md`
  area docs (the *what*), and `decisions/` (the *why* — today's ADRs, reframed),
  with `decisions/spikes/` and `decisions/archive/` under it. Full tree in the
  spike's "The doc tree" section.
- **Naming.** `<class><number>-<name>.md`. Decision docs keep their existing
  4-digit ADR numbers (so "ADR 0022" references still resolve conceptually);
  rendered titles come from `SUMMARY.md` link text and the `# H1`, not the
  filename.
- **Worked example exists.** [`docs/architecture/README.md`](../architecture/README.md)
  (the map) and [`docs/architecture/cosim-runtime.md`](../architecture/cosim-runtime.md)
  (one area's *what*), written against ADRs 0013/0017/0022. Read these as the
  template for the other five area docs.
- **The D2 diagram system is complete.** Wired into `book.toml` (elk layout) and
  CI; theme-adaptive via `theme/d2-diagrams.css`; documented in
  [`docs/architecture-diagrams.md`](../architecture-diagrams.md). Do not revisit
  it — just reuse it in the new area docs.

## Open follow-ups (priority-ordered)

### 1. Get the two open decisions ratified (blocks everything else)

- **Immutable vs living decision log.** The redesign *reverses* part of the
  living-ADR convention committed earlier this session (`0704c2f0`): once the
  reference layer carries current state, the decision log reverts to
  immutable/past-tense (a changed decision is a new record that supersedes, not an
  in-place rewrite). The user helped set the living rule, so this needs an explicit
  yes. It does **not** revive dated "Amendment" blocks — both models reject those.
- **The six-area taxonomy**, and specifically the two ambiguous placements: cell
  metadata (0010, 0011) sits between PDK enablement and timing; X-propagation
  (0016) spans the engine and cosim. Pick a home for each. Table of all 22 ADRs →
  6 areas is in the spike's "Taxonomy" section.

### 2. Execute the migration (large; do in reviewable slices)

Suggested order once §1 is settled:

1. **Audit the cross-reference surface first.** 74 files reference `docs/adr/` or
   "ADR NNNN" (per the verification grep). Per the global renaming rule, enumerate
   every location — docs, `CLAUDE.md`, `SUMMARY.md`, plan docs, CI, commit-message
   conventions — *before* moving anything. A scout/grep pass producing the full
   list is the right first step.
2. **Create `docs/architecture/`** and `git mv docs/adr/ docs/architecture/decisions/`,
   `git mv docs/spikes/ docs/architecture/decisions/spikes/`. Decide the
   filename form (`dec-0022-*.md` vs keeping `0022-*.md`) — the spike proposes the
   `dec-` prefix but it multiplies the link churn, so this is a real cost/benefit
   call.
3. **Write the five remaining `arch-N-*.md` area docs**, mirroring
   `cosim-runtime.md`: present-tense current state, each section forward-linking to
   its decision doc, an `## Implementation status` for the unbuilt, a `d2` diagram
   where a 2-D shape earns one.
4. **Reframe the decision docs** per the ratified immutable-vs-living call: strip
   current-state prose that now lives in the arch doc, past-tense the rationale,
   add a forward-link to the arch doc, fold or archive dated amendments.
5. **Rewrite the nav** (`SUMMARY.md`) to the tree in the spike, and fix all 74
   cross-references.
6. **Update `CLAUDE.md`** (it points at `docs/adr/` in several places) and any CI
   path assumptions (`check_doc_links.py`, the docs job).

### 3. Housekeeping

- Move [`docs/architecture-diagrams.md`](../architecture-diagrams.md) under the new
  structure if it fits better there (it's currently top-level).
- Consider whether this migration wants its **own branch off `main`** rather than
  riding `feat/cosim-signal-stream-tap` — it's orthogonal to the signal-stream
  feature, and the branch has accumulated a lot of doc-only work already.

## Critical context

- **Never do a partial rename.** 74 files is past the point where a missed
  reference is easy to spot; the link check catches rendered-page links but not
  prose "ADR 0022" mentions or `CLAUDE.md`. Audit-then-move.
- **Decision numbers are stable IDs.** Whatever the filename becomes, the 4-digit
  number must survive so historical references stay meaningful.
- **The `git log` outage.** GitHub's Actions API was in a sustained 503 outage
  through this session, so the docs-job CI result for the D2 changes was never
  confirmed green (local `mdbook build` + link check + actionlint all pass). First
  thing worth doing next session: confirm the latest run on
  `feat/cosim-signal-stream-tap` is green, now that the API should be back.
- **Diagram styling is verified via headless Chrome, not D2 PNGs** — a PNG shows
  the baked palette, not the themed result. Recipe is in
  `docs/architecture-diagrams.md`.

## References

- [`docs/spikes/architecture-doc-redesign.md`](../spikes/architecture-doc-redesign.md) — the full design, taxonomy, doc tree, and the field survey behind it
- [`docs/architecture/cosim-runtime.md`](../architecture/cosim-runtime.md) — the worked example to mirror
- [`docs/architecture-diagrams.md`](../architecture-diagrams.md) — the diagram system how-to
- [`docs/adr/README.md`](../adr/README.md) — where the three-bucket claim model currently lives (moves to `decisions/README.md`)
- [`docs/handoff-discipline.md`](../handoff-discipline.md) — how to resolve this handoff (fold into the migrated docs, then delete)

---

**Resume in a new session with:**
```
/resume_handoff docs/handoffs/architecture-docs-migration-handoff.md
```
