# Architecture Decision Records

ADRs capture decisions worth understanding later: the context, the
options considered, and the rationale for the choice. They are
numbered, append-only, and never silently rewritten. When reality moves
past an ADR, **record the change in the ADR rather than letting a stale
claim mislead** — two paths depending on the size of the change:

- **Full reversal** (the decision no longer holds) → supersede the old
  ADR with a new one and set the old status to **Superseded**.
- **Refinement** (a claim turned out too blunt, a constraint relaxed,
  a detail corrected) → add a dated **Amendment** note at the top of the
  affected section stating the current understanding, and **keep the
  original decision text in place below it** (relegated, not deleted),
  so the audit trail stays intact. Mark the status `Accepted (amended
  <date>)` and note it in the index. [Decision 0006](0006-sdf-preprocessing-model.md)
  and [Decision 0014](0014-aig-as-simulation-ir.md) are worked examples.

## Status legend

- **Accepted / Approved** — current, in effect.
- **Accepted (partial)** — design ratified and partly built; the ADR
  carries an `## Implementation status` section (see below).
- **Proposed** — drafted, not yet ratified.
- **Superseded** — historical, replaced by a later ADR or by a spike
  outcome; kept for the audit trail.

## Keeping status honest

An ADR's **Status is a claim about the codebase**, not an aspiration.
Before setting or changing it, **verify the claim against the
implementation** — read the code; don't trust the previous status or a
feature's "done" framing. The same goes for any present-tense statement
*inside* an ADR ("jitter feeds the setup/hold checker"): it's a
verifiable claim, so check it.

- **Don't bump Proposed → Accepted just because a design merged.**
  Confirm the decision is actually in effect in the code.
- When a design is ratified but only **partly built**, use
  **`Accepted (partial)`** and add an **`## Implementation status`**
  section splitting *implemented* (with file references) from *deferred*
  (with the specific gap). [Decision 0012](0012-cdc-jitter-injection.md) is
  the worked example.
- **Deferred work gets a home:** a plan under `docs/plans/` and a
  tracking issue, cross-linked from the ADR's status section, so the
  unbuilt half isn't lost.

This extends to user-facing docs and `--help` text: a sentence telling
the reader how the tool behaves is a verifiable claim — check it against
the code before writing it.

## Three kinds of claim, aged differently

An ADR is written against a codebase it usually changes, often *because* of
the ADR. To stay readable once that change lands, keep three kinds of claim
distinct, because they age at different rates:

1. **Why the decision was made** — the rationale, and the problem that
   prompted it. This is the ADR's lasting value; the code and `git log` show
   the *what*, only the ADR holds the *why*. Write the motivating facts as the
   state at decision time (past tense, or "when this was written") and don't
   rewrite them when the code moves. The problem is worth keeping even after
   it's solved.
2. **What the current architecture is** — what the code does today. A
   verifiable present-tense claim (see [Keeping status honest](#keeping-status-honest)),
   kept in step with the code. This is what goes stale when the code changes,
   and what an edit-in-place update targets. Each "what" should link back to the
   "why" that produced it, so a reader who lands on the current shape can reach
   the reasoning without hunting for it.
3. **What's decided but not built yet** — the gap between the decision and the
   code. Lives in `## Implementation status`; each piece moves into (2) as it
   lands, with a plan or issue so the unbuilt half isn't lost.

The failure mode is present-tensing all three the same way, so a future reader
can't tell whether a sentence is *why we did it*, *what the code does*, or
*what we still intend*. "`BATCH_SIZE` is 1024" reads as current fact; "today's
fixed `BATCH_SIZE` is what this replaces" reads as the starting point the
decision moves past. A `Proposed` ADR is the sharp case: everything it proposes
is bucket 3, so an early Implementation-status note ("nothing built; the code
today is X") lets the body describe the proposed design in the present without a
reader mistaking it for current reality.

## Index

| # | Title | Status |
|---|---|---|
| [0001](0001-opensta-as-oracle.md) | OpenSTA as the timing correctness oracle and sole STA path | Accepted (amended 2026-06-25; scope expanded 2026-05-01) |
| [0002](0002-timing-ir.md) | Timing intermediate representation | Accepted (amended 2026-06-25) |
| [0003](archive/0003-opentimer-primary-sta.md) | OpenTimer as in-process reference STA | Superseded (2026-05-01) — spike failed; OpenSTA subprocess only |
| [0004](0004-private-pdk-testing.md) | Private PDK testing track | Accepted (amended 2026-06-25) |
| [0005](0005-opensta-vendoring-and-corpus.md) | OpenSTA vendoring and test-corpus strategy | Accepted (amended 2026-06-25) |
| [0006](0006-sdf-preprocessing-model.md) | SDF preprocessing model and interim-to-release cutover | Accepted (amended 2026-05-02) |
| [0007](0007-timing-model-fidelity-roadmap.md) | Timing model fidelity roadmap | Proposed (line refs amended 2026-06-25) |
| [0008](0008-structured-timing-output.md) | Structured timing output as first-class deliverable | Accepted (amended 2026-06-25) |
| [0009](0009-opensta-verilog-reader-inputs.md) | OpenSTA Verilog reader inputs | Accepted (amended 2026-06-25) |
| [0010](0010-declarative-cell-metadata.md) | Declarative cell metadata | Accepted (amended 2026-06-25) |
| [0011](0011-ram-port-mapping-schema.md) | RAM port-mapping schema for declarative cell metadata | Accepted (amended 2026-06-25) |
| [0012](0012-cdc-jitter-injection.md) | Reproducible CDC jitter injection for multi-clock cosim | Accepted (partial; amended 2026-06-25) |
| [0013](0013-plural-peripheral-configs.md) | Cosim peripheral model architecture | Accepted (amended 2026-06-25) |
| [0014](0014-aig-as-simulation-ir.md) | AIG as simulation intermediate representation | Accepted (amended 2026-06-25) |
| [0015](0015-boomerang-execution-model.md) | Boomerang execution model and GPU resource mapping | Accepted |
| [0016](0016-selective-x-propagation.md) | Selective X-propagation | Accepted (amended 2026-06-25) |
| [0017](0017-cosim-execution-model.md) | Cosim execution model | Accepted (amended 2026-06-25) |
| [0018](0018-distribution-and-installation.md) | Distribution and installation model | Accepted (amended 2026-06-25) — Phase 4 & 7 open |
| [0019](0019-cell-model-ir.md) | Cell-model IR: a complete per-cell-type library descriptor | Proposed |
| [0020](0020-python-engine-binary-wheel.md) | Python engine as a bundled binary wheel (cibuildwheel) | Draft — deferred (PyO3 preferred; see ADR) |
| [0021](0021-behavioral-rtl-support.md) | Behavioral RTL support via an embedded synthesis front-end (YoWASP) | Proposed |
| [0022](0022-flow-controlled-io.md) | Flow-controlled external I/O across the batch boundary | Proposed |

## How the ADRs relate

- **0014 / 0015** document the core simulation pipeline: 0014
  explains why the AIG (and-inverter graph) is the simulation IR —
  its uniform AND-gate structure enables the boomerang reduction tree
  and eliminates per-cell dispatch in the GPU kernel.  0015 describes
  the boomerang execution model itself — the 13-level hierarchical
  reduction tree, the GPU resource limits it imposes (8191 inputs,
  8191 outputs, 4095 intermediates, 64 SRAM groups per partition),
  the hypergraph partitioning that distributes work across GPU blocks,
  and the packed instruction format (`FlattenedScriptV1`) consumed by
  the kernel.  Together they document the path from gate-level
  Verilog to GPU kernel execution that the GEM paper describes.

- **0001 / 0003 / 0005 / 0006** describe the timing oracle stack:
  OpenSTA as the ground truth (0001), vendored at a pinned revision
  with its own corpus reused (0005), driving SDF preprocessing
  out-of-process (0006). The earlier OpenTimer in-process plan (0003)
  was retired after the spike ([`spikes/opentimer-sky130.md`](spikes/opentimer-sky130.md)).
- **0002** is the data contract those tools talk over — a JSON timing
  IR consumed by Jacquard, produced by `opensta-to-ir`.
- **0004** governs how PDK-specific testing happens for NDA-bound
  contributors without leaking files into the public repo.
- **0007 / 0008** are the forward-looking pair: 0008 (Approved)
  defines the structured timing output Jacquard owes downstream
  flows; 0007 (Proposed) sketches the model-fidelity work needed to
  back those outputs at scale (δ(T), clock-tree skew, wire delay).
  Scheduling for both lives in
  [`../plans/post-phase-0-roadmap.md`](../../plans/post-phase-0-roadmap.md).

- **0013 / 0017** cover the cosim runtime: 0013 documents the
  peripheral model architecture (CPU-side `PeripheralModel` trait,
  GPU-side kernel patterns, ring buffers, plural-config convention);
  0017 documents the execution model (batch dispatch loop,
  multi-clock scheduler, edges-vs-cycles semantics).
- **0016** accepts the selective X-propagation design documented in
  [`docs/selective-x-propagation.md`](../../selective-x-propagation.md).
  The full seven-phase design lives there; the ADR is a thin
  acceptance record with a summary of key choices.

## Adding a new ADR

1. Pick the next number (highest existing + 1).
2. Filename: `NNNN-short-kebab-title.md`.
3. Start with `# ADR NNNN — <title>` and a `**Status:**` line — set it
   to match the code, not the intent (see [Keeping status honest](#keeping-status-honest)).
4. Standard sections: Context, Decision, Consequences. Add Amendment
   blocks dated when the decision is revisited; do not rewrite
   accepted history.
5. Add the row to the table above.
