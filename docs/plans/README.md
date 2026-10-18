# Implementation Plans

Phased implementation plans with entry and exit criteria. Plans live
here when the work spans multiple commits and needs an explicit
scheduling artefact. Once shipped, the plan moves to `archive/` as a
historical record (Status flipped to *Implemented*) rather than being
deleted, so the phasing is recoverable later.

For short-lived working memory between sessions, see
[`../handoff-discipline.md`](../handoff-discipline.md) — that lives
in `docs/handoffs/` and is deliberately kept separate from the
persistent plans here.

## Status legend

- **Active** — currently being worked on or scheduled.
- **Implemented** — shipped; kept as historical record.
- **Deferred** — captured for future work; not currently scheduled.
- **Exploratory** — architectural thinking captured ahead of demand.

## Index

| Plan | Status |
|---|---|
| [Post-Phase-0 Roadmap](post-phase-0-roadmap.md) | Active — scheduling doc for Decisions 0007 and 0008 |
| [GF180MCU PDK enablement](gf180mcu-enablement.md) | Mostly implemented — Phases 0–6 shipped; Phase 7 deferred |
| [WS3 follow-up: re-add cosim `--sdf` via `opensta-to-ir`](ws3-cosim-sdf-followup.md) | Deferred |
| [Multi-clock and stimulus architecture](multi-clock-and-stimulus-architecture.md) | Exploratory — demand-driven |
| [Cosim backend portability](cosim-backend-portability.md) | Active — design captured (#105); see Decision 0017 amendment |
| [RTL on-ramp folded into `sim`/`cosim`](rtl-onramp-sim-integration.md) | Active — reworks #167, realises Decision 0021 (#162) |

## Archive

Shipped plans move to `archive/` so this directory lists only live
work. They stay in the repo because decision records cite them as the record of
how each decision was carried out.

| Plan | Status |
|---|---|
| [Phase 0: Timing IR and OpenSTA oracle](archive/phase-0-ir-and-oracle.md) | Implemented |
| [WS2: `opensta-to-ir`](archive/ws2-opensta-to-ir.md) | Implemented |
| [WS3: delete SDF parser + interim runtime hook](archive/ws3-delete-sdf-parser.md) | Implemented (see Decision 0006 Amendment) |
| [Declarative cell metadata](archive/declarative-cell-metadata.md) | Implemented |
| [Selective X-propagation in cosim](archive/cosim-xprop.md) | Implemented |
| [Cell-model IR](archive/cell-model-ir.md) | Largely delivered; realises Decision 0019 |
| [RTL-source provenance](archive/rtl-source-provenance.md) | Shipped in v0.3.0; realises Decision 0021 Phase 2 |

## Reading order for new contributors

If you want to understand how the timing stack got to where it is:

1. [`phase-0-ir-and-oracle.md`](archive/phase-0-ir-and-oracle.md) — the
   umbrella plan, with the five work streams (WS1–WS5).
2. [`ws2-opensta-to-ir.md`](archive/ws2-opensta-to-ir.md) and
   [`ws3-delete-sdf-parser.md`](archive/ws3-delete-sdf-parser.md) — the
   per-work-stream detail for the IR producer and the SDF parser
   removal.
3. [`post-phase-0-roadmap.md`](post-phase-0-roadmap.md) — what comes
   next, sequenced against the decisions.

## Adding a new plan

1. Filename: short kebab-case (`<topic>.md` or
   `<ws-or-phase>-<topic>.md`).
2. Start with `# Plan — <title>` and a `**Status:**` line.
3. Where the plan executes a specific decision or work stream, name them
   in a `**Predecessors:**` / `**decisions:**` block near the top so the
   dependency graph is explicit.
4. Add the row to the table above.
5. When the plan ships, flip its status, move its row to the Archive
   table, and move the file with the doc-move tool, all in one commit:

   ```sh
   python3 docs/scripts/move_doc.py docs/plans/<plan-name>.md docs/plans/archive/
   ```

   The tool `git mv`s the plan into `archive/` and repoints every link and
   `docs/plans/` path in the repo, including source comments. Move its
   `SUMMARY.md` entry under "Completed plans" by hand, then run
   `python3 docs/scripts/check_doc_links.py`, which fails if you forget.
