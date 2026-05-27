# Architecture Decision Records

ADRs capture decisions worth understanding later: the context, the
options considered, and the rationale for the choice. They are
numbered, append-only, and never silently rewritten — if a decision
changes, supersede the old ADR with a new one and update the status.

## Status legend

- **Accepted / Approved** — current, in effect.
- **Proposed** — drafted, not yet ratified.
- **Superseded** — historical, replaced by a later ADR or by a spike
  outcome; kept for the audit trail.

## Index

| # | Title | Status |
|---|---|---|
| [0001](0001-opensta-as-oracle.md) | OpenSTA as the timing correctness oracle and sole STA path | Accepted (scope expanded 2026-05-01) |
| [0002](0002-timing-ir.md) | Timing intermediate representation | Accepted |
| [0003](0003-opentimer-primary-sta.md) | OpenTimer as in-process reference STA | Superseded (2026-05-01) — spike failed; OpenSTA subprocess only |
| [0004](0004-private-pdk-testing.md) | Private PDK testing track | Accepted |
| [0005](0005-opensta-vendoring-and-corpus.md) | OpenSTA vendoring and test-corpus strategy | Accepted |
| [0006](0006-sdf-preprocessing-model.md) | SDF preprocessing model and interim-to-release cutover | Accepted (amended 2026-05-02) |
| [0007](0007-timing-model-fidelity-roadmap.md) | Timing model fidelity roadmap | Proposed |
| [0008](0008-structured-timing-output.md) | Structured timing output as first-class deliverable | Approved |
| [0009](0009-opensta-verilog-reader-inputs.md) | OpenSTA Verilog reader inputs | Accepted |
| [0010](0010-declarative-cell-metadata.md) | Declarative cell metadata | Accepted |
| [0011](0011-ram-port-mapping-schema.md) | RAM port-mapping schema for declarative cell metadata | Accepted |
| [0012](0012-cdc-jitter-injection.md) | Reproducible CDC jitter injection for multi-clock cosim | Proposed |
| [0013](0013-plural-peripheral-configs.md) | Cosim peripheral model architecture | Proposed |
| [0014](0014-aig-as-simulation-ir.md) | AIG as simulation intermediate representation | Accepted |
| [0015](0015-boomerang-execution-model.md) | Boomerang execution model and GPU resource mapping | Accepted |
| [0016](0016-selective-x-propagation.md) | Selective X-propagation | Accepted |
| [0017](0017-cosim-execution-model.md) | Cosim execution model | Accepted |

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
  was retired after the spike ([`../spikes/opentimer-sky130.md`](../spikes/opentimer-sky130.md)).
- **0002** is the data contract those tools talk over — a JSON timing
  IR consumed by Jacquard, produced by `opensta-to-ir`.
- **0004** governs how PDK-specific testing happens for NDA-bound
  contributors without leaking files into the public repo.
- **0007 / 0008** are the forward-looking pair: 0008 (Approved)
  defines the structured timing output Jacquard owes downstream
  flows; 0007 (Proposed) sketches the model-fidelity work needed to
  back those outputs at scale (δ(T), clock-tree skew, wire delay).
  Scheduling for both lives in
  [`../plans/post-phase-0-roadmap.md`](../plans/post-phase-0-roadmap.md).

- **0013 / 0017** cover the cosim runtime: 0013 documents the
  peripheral model architecture (CPU-side `PeripheralModel` trait,
  GPU-side kernel patterns, ring buffers, plural-config convention);
  0017 documents the execution model (batch dispatch loop,
  multi-clock scheduler, edges-vs-cycles semantics).
- **0016** accepts the selective X-propagation design documented in
  [`docs/selective-x-propagation.md`](../selective-x-propagation.md).
  The full seven-phase design lives there; the ADR is a thin
  acceptance record with a summary of key choices.

## Adding a new ADR

1. Pick the next number (highest existing + 1).
2. Filename: `NNNN-short-kebab-title.md`.
3. Start with `# ADR NNNN — <title>` and a `**Status:**` line.
4. Standard sections: Context, Decision, Consequences. Add Amendment
   blocks dated when the decision is revisited; do not rewrite
   accepted history.
5. Add the row to the table above.
