# Timing correctness

*Reference — the current design as it stands in the code. The why lives in the
decision records linked from each section; the not-yet-built work is under
[Implementation status](#implementation-status). Present tense means "true
today"; if a claim here is stale, the code moved and this doc didn't.*

Jacquard's timing story has two halves. Getting a number is the [simulation
engine](simulation-engine.md)'s job: per-gate delays ride the boomerang's
arrival propagation and come out as setup/hold reports. Trusting that number is
this doc's job — where the delays come from, how they're validated, and what
they mean. Static timing analysis (STA) is a solved problem; Jacquard doesn't
re-solve it. It treats an existing STA tool as ground truth and builds a
narrow, auditable pipe from that tool's output into the GPU kernel. The accuracy
tolerances and validation requirements this pipe must meet are the
[Timing Correctness Requirements](../timing-correctness.md) contract.

## OpenSTA as the oracle and the only STA path

**OpenSTA** is both jobs at once: the tool Jacquard's own numbers are checked
against, and the only static timing analyzer Jacquard talks to at all. An
earlier plan paired OpenSTA with an in-process reference engine (OpenTimer),
so a divergence between Jacquard and OpenSTA could be triangulated against a
third source; a spike found OpenTimer's input pipeline unfit for
OpenROAD-flow output, and that plan is superseded. Where Jacquard's results
disagree with OpenSTA's past a declared tolerance, Jacquard is wrong until
proven otherwise — the divergence gets fixed, justified in writing, or filed
as a bug. [Decision 0001](decisions/0001-opensta-as-oracle.md).

OpenSTA is GPL-3.0; Jacquard is permissively licensed, so nothing links it.
Every call to OpenSTA crosses a subprocess boundary, driven by
**`opensta-to-ir`**, a standalone preprocessing tool that also owns the
Liberty/Verilog/SDF/SPEF/SDC parsing on OpenSTA's side. `opensta-to-ir` may
be invoked ahead of time to produce a `.jtir` file, or subprocessed live by
`jacquard sim --sdf <path>` — both are permitted in the shipped runtime, as
long as OpenSTA itself is discovered on `PATH` rather than bundled into a
Jacquard release artefact. [Decision 0006](decisions/0006-sdf-preprocessing-model.md).

OpenSTA is vendored as a git submodule at `vendor/opensta/`, pinned to a
specific revision for CI reproducibility, but never built as part of
Jacquard — subprocess calls use whatever OpenSTA binary the environment
provides. The submodule's real payload is its test corpus. A small, curated
**primary regression corpus** (`tests/timing_ir/corpus/`) runs on every CI
job; a much larger **stress corpus**, referenced by manifest into the
submodule rather than duplicated, runs nightly to catch parser crashes and
malformed IR on dialects Jacquard doesn't otherwise see. As shipped, the
primary corpus holds one entry (`aigpdk_dff_chain`); SKY130/NVDLA/MCU-SoC
entries are pending on a CI SKY130-Liberty install strategy, and the stress
corpus's manifest is still empty. [Decision 0005](decisions/0005-opensta-vendoring-and-corpus.md).

## The timing IR

Everything OpenSTA computes for a design reaches Jacquard through one
channel: the **timing IR**, a schema-versioned FlatBuffers format with a JSON
sidecar for CI diffs and human inspection. `opensta-to-ir` is the producer;
`flatten.rs::load_timing_from_ir` is the consumer, turning per-cell arcs into
the `PackedDelay` (rise/fall picoseconds) and `DFFConstraint` (setup/hold)
structures the simulation engine bakes into its script. The IR is
multi-corner natively — values are min/typ/max across a declared PVT corner
set, selectable at sim time via `--timing-corner` — and every arc carries
provenance (`asserted` from the input file, `computed` by OpenSTA, or
`defaulted`). Unrecognised vendor annotations pass through as typed
`VendorExtension` blobs rather than being silently dropped. The IR's scope is
deliberately narrow: per-design timing annotation only, never a netlist
representation, a timing graph, or cell characterization.
[Decision 0002](decisions/0002-timing-ir.md).

Per-cell-*type* characterization — setup/hold, clock-to-Q, the numbers that
don't vary by instance — is a different axis and, today, comes from a
separate runtime path: `liberty_parser::TimingLibrary`, parsed from a `.lib`
file at sim startup. **Decision 0019** proposes folding that parse, plus a
cell's logic and sequential classification, into one generated **cell-model
IR** per library, with the simulation corner picked explicitly via
`--corner` rather than read from the netlist or SDF. That descriptor doesn't
exist yet; it's Proposed, not built — see
[Implementation status](#implementation-status).
[Decision 0019](decisions/0019-cell-model-ir.md).

## From SDF to a running gate

The hand-rolled `src/sdf_parser.rs` is gone. Every path that once consumed
SDF directly now consumes timing IR instead, which is what makes OpenSTA a
**required runtime dependency** for any timing-aware flow, not just a CI
concern: `jacquard sim --sdf <path>` and the standalone `opensta-to-ir` tool
both subprocess OpenSTA to produce the IR that `flatten.rs` then loads.

OpenSTA's own `read_verilog` accepts only structural Verilog — cell
instantiations and bare-net assigns, no RTL operators or bit-selects. Some
flows wrap a clean post-P&R netlist in a thin RTL shim (an active-low OEB
patch, for instance), which OpenSTA's reader rejects outright.
`opensta-to-ir` filters each `--verilog` input at invocation time, extracting
just the `module <top> … endmodule` block before handing it to OpenSTA, so
wrapper modules are simply never seen. What `opensta-to-ir` cannot check is
whether the Verilog handed to it is the *right stage* for a given SDF — a
pre-P&R synthesis netlist looks structurally identical to the post-P&R body
but is missing the hundreds of thousands of P&R-inserted cells the SDF
actually references, and OpenSTA quietly drops SDF entries whose endpoints
aren't in the loaded design. Picking the right netlist stays the caller's
job. [Decision 0009](decisions/0009-opensta-verilog-reader-inputs.md).

Once the IR is loaded, `flatten.rs` overlays it on the AIG: each cell's
delay lands on the AIG pin it drives, and `inject_timing_to_script` bakes
that value into the [flattened script](simulation-engine.md#the-flattened-script)'s
per-gate padding slot as picoseconds the kernel reads directly during
boomerang reduction. `DFFConstraint` values become the per-word setup/hold
check the kernel runs at each cycle boundary. See
[`../timing-simulation.md`](../timing-simulation.md) for the full mechanics
of that propagation, including the three independent sources of conservative
overestimate in today's packed-32 model (max-of-rise/fall per cell, max wire
delay across a cell's input pins, max arrival across the 32 signals sharing
a thread) — the model never under-reports a violation, which is what setup
checking needs, at the cost of some false positives.

## Structured output

Getting a timing number out of a run used to mean grepping stderr for a
state-word index. Symbolic violation messages now name the hierarchical
signal directly (`top/cpu/regs[7][bit 22]`); `--timing-report <path.json>`
emits a schema-versioned, end-of-run document with per-DFF worst arrival and
slack, a per-cycle violation list, and top-N worst-slack rankings even where
no violation occurred; `--timing-summary` is the fast human-readable
counterpart for scripts and quick inspection. All three ship for `sim` on
Metal, CUDA, and HIP. [Decision 0008](decisions/0008-structured-timing-output.md).

## Constraints

- **OpenSTA is the only STA path.** There is no in-process reference engine;
  a run that needs timing data needs an OpenSTA-produced (or pre-converted)
  IR. [Decision 0001](decisions/0001-opensta-as-oracle.md).
- **The Verilog fed to `opensta-to-ir` must be the post-P&R structural
  body**, matching what the SDF was generated against. `opensta-to-ir`
  strips RTL wrapper modules automatically; it cannot detect a
  wrong-design-stage substitution, since a synthesis netlist and a P&R
  netlist can share a module name while differing by orders of magnitude in
  cell count. [Decision 0009](decisions/0009-opensta-verilog-reader-inputs.md).
- **The timing model is conservative, not exact.** Rise/fall collapse, max
  wire delay per cell, and max arrival per packed thread all push arrival
  times up, never down. See [`../timing-simulation.md`](../timing-simulation.md).
- **Timed (arrival-annotated) cosim runs on Metal only**, and the structured
  `--timing-report` JSON is `sim`-only; the [cosim runtime](cosim-runtime.md)
  hasn't wired either through yet.
- **Synchronous, edge-triggered logic only**, inherited from the
  [simulation engine](simulation-engine.md#constraints) — there's no timing
  model for a level-sensitive latch or asynchronous sequential feedback to
  annotate in the first place.
- **No linking of GPL code, ever**, and no bundling of OpenSTA in a Jacquard
  release artefact. Subprocess invocation of a user-installed OpenSTA is the
  only sanctioned integration path. [Decision 0006](decisions/0006-sdf-preprocessing-model.md).

## Implementation status

Built and in use: OpenSTA as the sole out-of-process STA oracle; the
FlatBuffers + JSON timing IR with multi-corner support and provenance
tagging; the `opensta-to-ir` subprocess path (both pre-converted `--timing-ir`
and live `sim --sdf`); the structural-Verilog wrapper filter; per-gate delay
and per-DFF setup/hold injection into the flattened script; GPU-side
setup/hold violation detection on Metal, CUDA, and HIP for `sim`; and the
structured output surface (symbolic violations, `--timing-report`,
`--timing-summary`, worst-slack ranking) for those same three backends.
Pillar B (per-DFF clock-tree skew via a `ClockArrival` IR table, folded into
the setup/hold check) is also landed, ahead of its own roadmap's schedule.

Decided but not yet built:

- **Native Rust SDF→IR converter** (Phase 3), which would remove the
  OpenSTA subprocess from the shipped runtime entirely. No longer
  release-gating; deferred indefinitely until bandwidth or commercial demand
  reopens it. [Decision 0006](decisions/0006-sdf-preprocessing-model.md).
- **`cosim`'s structured timing report and timed simulation on
  CPU/CUDA/HIP.** Today only Metal cosim carries arrival annotations, and
  none of the `--timing-report`/`--timing-summary` surface reaches `cosim`.
  [Decision 0008](decisions/0008-structured-timing-output.md).
- **The cell-model IR** (Decision 0019) — Proposed, not started. Per-cell
  timing characterization stays on the runtime `liberty_parser::TimingLibrary`
  path until this lands; `--corner`-based cell-library selection doesn't
  exist yet. [Decision 0019](decisions/0019-cell-model-ir.md).
- **The timing-model fidelity roadmap** (Decision 0007) — Proposed. Of its
  three pillars, only Pillar B (clock-tree skew, above) has landed. Still
  open: Pillar C Tier 1 (per-receiver wire delay, gated on Decision 0007
  acceptance), Pillar A (dynamic per-gate delay, δ(T), gated on per-cell
  SPICE characterisation and explicitly scheduled after Pillars B and C),
  and Pillar C Tiers 2–3 (inter-partition wire delay, NoC-aware partitioning
  hints). Scheduling detail in
  [`../plans/post-phase-0-roadmap.md`](../plans/post-phase-0-roadmap.md).
- **Decision 0008's optional outputs** — arrival histograms, an
  OpenSTA-critical-path cross-reference, and DFF path-back-trace. Scheduled
  per user demand once the required items above see use.

## Decisions behind this

- [Decision 0001](decisions/0001-opensta-as-oracle.md) — OpenSTA as the
  timing-correctness oracle and the sole STA path; the in-process
  alternative (OpenTimer) was superseded by a spike.
- [Decision 0002](decisions/0002-timing-ir.md) — the timing IR: FlatBuffers
  + JSON, multi-corner, provenance-tagged, per-design annotation only.
- [Decision 0005](decisions/0005-opensta-vendoring-and-corpus.md) — OpenSTA
  vendored as a pinned submodule; primary vs. stress test corpus split.
- [Decision 0006](decisions/0006-sdf-preprocessing-model.md) — the SDF
  preprocessing model: deleting the hand-rolled parser, `opensta-to-ir` as
  the subprocess bridge, and the (now relaxed) rules on runtime subprocess
  invocation.
- [Decision 0008](decisions/0008-structured-timing-output.md) — structured
  timing output as a first-class deliverable: symbolic violations,
  `--timing-report`, `--timing-summary`, worst-slack ranking.
- [Decision 0009](decisions/0009-opensta-verilog-reader-inputs.md) —
  OpenSTA's structural-only Verilog reader, and where the wrapper-stripping
  and design-stage responsibilities sit.
- [Decision 0019](decisions/0019-cell-model-ir.md) — Proposed: a generated
  cell-model IR unifying per-cell-type logic, classification, and L4 timing
  characterization, replacing the runtime Liberty parse.
- [Decision 0007](decisions/0007-timing-model-fidelity-roadmap.md) —
  Proposed: the three-pillar roadmap (dynamic delay, clock-tree skew, wire
  delay at scale) for closing the accuracy gap with an event-driven
  reference simulator.
