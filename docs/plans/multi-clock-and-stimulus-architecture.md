# Multi-clock and stimulus architecture — exploratory roadmap

**Status:** Captured architectural thinking. Most phases here are
demand-driven and will only be picked up when a real-world workload requires
them. Phases 1 and 2 may be worth scheduling on their own merits in a future
release; the rest are written down so the design space is on record when the
need appears.

This is a **design-space** doc, not a scheduling doc. It complements
`post-phase-0-roadmap.md` (which schedules committed work) by capturing the
architecture for two related areas — multi-clock-domain support and stimulus
generation — that today have working but limited implementations.

## Why now

The conversation that produced this doc was about supporting cosim against
external testbench environments (UVM, CocoTB) and external clock sources (PHY,
audio, DFS). Two observations crystallised the architecture:

1. **Real designs partition into large synchronous islands with thin
   boundaries.** External-clock and DFS scenarios look intractable until you
   notice that <1 % of nets typically cross domains; the bulk of the design is
   batchable inside one island.
2. **Stimulus generation and stimulus consumption don't have to share a
   loop.** Today cosim couples the testbench tick-by-tick to the GPU dispatch.
   Decoupling them — via streaming or full precompute — turns the GPU from a
   ping-ponging coprocessor into a stream consumer.

Both observations point at architecture changes that compose cleanly with each
other, with the existing multi-clock plumbing, and with the existing X-prop /
timing-arrival infrastructure.

## What exists today

Worth pinning down so the gap is precise:

- **Multi-clock-domain functional support.** `MultiClockScheduler` in
  `src/sim/cosim_metal.rs:1347` builds a tick-by-tick edge schedule over the
  LCM of all domains' periods (with GCD granularity). DFFs are tagged by clock
  domain via `clock_pin2aigpins` in `src/aig.rs:209`. Each scheduler tick
  asserts only the firing domains' posedge/negedge flag bits; the GPU kernel
  gates DFF write-back on those flags, so non-firing domains' DFFs hold.
- **LCM constraint.** The scheduler asserts `schedule_len <= 1_000_000`
  (`cosim_metal.rs:1376`). Commensurable periods (PLL-derived) work; truly
  non-commensurable external clocks (audio, USB-recovered, DFS-mid-flight) hit
  the cap.
- **Cosim stimulus.** `InputDispatcher` (`src/sim/input_stim.rs`) consumes a
  chipflow-compatible `wait`/`action`/`stop` JSON command list. Peripheral
  models (`src/sim/models/`) drain queued actions per edge and emit events.
  Generation is interleaved with the GPU dispatch loop — every tick (or every
  few ticks) round-trips through the host.
- **VCD replay path.** `jacquard sim` already runs from a precomputed input
  VCD with no host-side reactive logic. This is, in effect, the "Level 1"
  precomputed-edge mode described below; the gap is between cosim's reactive
  loop and sim's flat replay, not in the kernel itself.
- **CDC checking.** None today. SDF setup/hold checks exist
  (`src/timing_report.rs`) but are not wired through any CDC-specific path.

## Architecture: two orthogonal axes

The work falls cleanly into two independent dimensions.

### Axis 1 — Spatial: synchronous islands with thin boundaries

A static analysis pass partitions the AIG into **islands**: maximal connected
sets of gates whose transitive fanin/fanout stays inside one clock domain.
Whatever's left is the **boundary** — combinational gates and DFFs whose data
cones cross domains. In real designs the boundary is small, dominated by
synchronizers (2FF), async FIFO control, and handshake glue.

Per-island execution lets the GPU:

- Skip evaluation of an island whose state hasn't changed.
- Batch K consecutive ticks of a fast island into one kernel launch when the
  slow island has no edges in the window.
- Treat the boundary as a small mailbox (source-island outputs read by
  destination-island reads) rather than a global state vector.

This is essentially **functional partitioning** for parallel discrete-event
simulation, but the GPU dataflow model gets *more* benefit than a CPU sim
because batched dataflow is exactly what a fast island's run-ahead window
wants.

### Axis 2 — Temporal: stimulus generation decoupled from consumption

The cosim host loop is the throughput floor today. Decoupling has three
levels:

- **Replay** — the testbench has already produced a complete input VCD; the
  GPU just plays it back. Today's `jacquard sim` is this case.
- **Streaming buffer** — testbench runs in a separate thread feeding a ring
  buffer of `(tick, input_op)` tuples. GPU consumes batches. As long as the
  producer keeps up *on average*, the GPU never stalls. Works because most
  ticks have no input change and peripheral state machines run far slower than
  the kernel.
- **Record-and-replay with divergence detection** — pass 1 runs full cosim and
  records every input transition; pass 2 replays at line-rate while
  checksumming outputs against the recorded run. If outputs diverge, abort and
  fall back. Wins decisively for regression CI where most runs confirm
  "nothing externally observable changed".

## Phase breakdown

Each phase is independently shippable. The phase numbering here is local to
this doc and should not be confused with the timing-IR phase numbering in
`post-phase-0-roadmap.md`.

| Phase | Topic | Trigger |
|---|---|---|
| **MC.1** | Static island partitioner (analysis only, emits metadata) | Standalone-useful for CDC reporting; could land in a future release without further work |
| **MC.2** | Min-heap multi-clock scheduler (replaces LCM precompute) | First non-commensurable external clock or DFS use case lands |
| **MC.3** | Streaming stimulus buffer (decouples testbench thread from kernel) | First workload where cosim CPU↔GPU round-trip is measured as the bottleneck |
| **MC.4** | Per-island kernel dispatch + multi-rate batching | MC.1+MC.2 in place; first multi-domain workload large enough that whole-AIG eval per tick is wasteful |
| **MC.5** | Record-and-replay with divergence detection | Regression CI throughput becomes a release blocker |
| **MC.6+** | Speculation staircase, AOT trace compilation, profile-guided kernel specialization | Demand-driven; deferred until measurement shows residual sync overhead after MC.4 |

### MC.1 — Static island partitioner

Walk the AIG; for each gate compute the set of clock domains its transitive
fanin/fanout touches. Tag gates as **island-internal** (fanin and fanout both
inside one domain) or **boundary** (touches more than one domain on either
side). Emit per-island gate counts and a list of boundary gates as metadata
on the existing `FlattenedScript`.

What it enables on its own, even with no runtime change:

- Diagnostic: "this design has 14 inter-domain combinational paths from
  `audio_clk` → `core_clk` and 2 the other way". Useful for designers
  reviewing CDC structure.
- Data structure that MC.2 / MC.4 / CDC reporting all need.
- Sanity-check on the "<1 %" boundary-surface assumption for the workloads
  that motivate further phases.

Classification policy for derived signals (e.g. a sync-FIFO read pointer in
`clock_b` qualified by an output of a sync chain from `clock_a`): classify
aggressively. Only gates whose *direct* fanin includes pins from multiple
domains are boundary; downstream gates fed by a domain-tagged
pre-synchronizer output inherit that domain. This pushes the boundary in as
close to the structural CDC crossing as possible and is what makes the
"<1 %" claim hold on real designs — a lazy classification that propagated
"multi-domain" forward through every downstream cone would yield a
boundary surface that swallowed half the design.

Code locations: extends `aig.rs` (domain analysis on `DriverType`) and
`flatten.rs` (metadata on `FlattenedScriptV1`). No kernel changes.

### MC.2 — Min-heap multi-clock scheduler

Replace `MultiClockScheduler`'s precomputed `Vec<TickEdges>` with a min-heap
of `(next-edge-time, domain)` pairs. Pop the next edge, dispatch, push the
domain's next edge back. No LCM constraint; non-commensurable periods are
free. DFS support falls out: when the DUT writes a clock-control register,
the host updates the heap entry's period.

DFS hook design: explicit, not generic signal-watching. The cosim config
declares `(control_signal, period_table)` pairs; the host polls the named
bit each tick (cheap — one bit) and updates the heap. Generic
"call-back-on-arbitrary-signal" is rejected as too coupled.

Code locations: `MultiClockScheduler::new` and `build_edge_ops` in
`cosim_metal.rs`. Same per-domain flag emission, different scheduling
backend.

### MC.3 — Streaming stimulus buffer

`InputDispatcher` becomes a trait; today's `FileDispatcher` is one
implementation. New implementations:

- `ThreadedDispatcher` — runs peripheral models on a separate thread; emits
  `(tick, input_op)` into a lock-free SPSC ring buffer; GPU loop consumes
  batches.
- `StreamDispatcher` — same shape but the producer is a JSON-lines stream
  over a Unix socket / stdio (this is also the bridge to UVM/CocoTB peer
  testbenches).

Latency budget: the producer must be at least one tick ahead of the
consumer. For transaction-level workloads this is easy (peripheral state
machines run orders of magnitude slower than the GPU). For sub-cycle
reactive loops it isn't, and those workloads stay on the synchronous path.

Code locations: refactor `input_stim.rs` around a trait; new module for
ring-buffer plumbing; cosim main loop drains a batch per dispatch instead of
one tick.

### MC.4 — Per-island kernel dispatch + multi-rate batching

Build per-island execution scripts (and one boundary script) from the
metadata MC.1 produces. Cosim main loop becomes:

```rust
loop {
    let (next_t, domain) = scheduler.peek();
    let lookahead = scheduler.next_other_domain_edge(domain) - now;
    let edges_in_window = lookahead / domain.period;
    dispatch(island_script[domain], edges = edges_in_window);
    dispatch(boundary_script);  // only if boundary signals changed
    advance_clock(now + edges_in_window * domain.period);
}
```

Boundary mailbox lives in shared state-buffer slots that the source island's
script writes and the destination island's script reads. Repcut continues to
partition each island's script across GPU blocks independently.

Tight-boundary gates (combinationally fed by both domains) force a sync
point on every edge of either side; MC.1's metadata identifies these so the
runtime knows when batching can extend.

### MC.5 — Record-and-replay with divergence detection

Add `--record-stimulus` to cosim that emits a complete tick-by-tick input
VCD and a per-tick output checksum. Add `--replay-stimulus` to sim (or a
new mode) that consumes the VCD, runs at line-rate, and verifies the
checksum each batch.

Divergence handling is two-tier, not just abort:

- Mismatch in **watched signals** (the existing cosim `signals_of_interest`
  set, or a `--watch` CLI argument) → abort and require re-recording. This
  is the genuine "the design's externally observable behaviour changed"
  case — the recording is now stale and replay is unsafe.
- Mismatch in **unwatched signals** → warn-and-continue against the
  recorded transitions. Internal microarchitectural changes that don't
  move the observable surface are normal during development; aborting on
  them defeats the purpose of accelerating regression CI, where most runs
  exist to confirm "nothing externally observable changed".

The watchset is the user-visible policy lever — it specifies what
"externally observable" means for this design. Default to the cosim
output signals (the natural CI invariant) plus any user-declared
checkpoint signals.

Useful primarily as a regression-CI accelerator. Doesn't help one-off
runs.

**Cross-test sharing.** A single design accumulates many test cases. The
natural extension of record-and-replay is to share the design-side
specialized kernel across all tests in the suite and vary only the
stimulus recording. For a suite of N tests against one design, recording
costs N× pass-1 (one per test, on demand or in parallel) but replay costs
N× line-rate-kernel-launches sharing the same compiled state-buffer
layout. That's a multiplicative win on top of per-test record-and-replay
and is the actual leverage point for full-suite CI throughput.

### MC.6+ — Deferred sophistication

Documented now so the design space is on record:

- **Speculation staircase** for hot boundaries: value prediction → protocol
  pattern recognition → control-slice reachable-set enumeration → full
  case enumeration. Each tier larger and cheaper-to-skip. Add a "case"
  dimension to the kernel dispatch only if measured sync overhead after
  MC.4 justifies it.
- **AOT trace compilation**: when stimulus is fully known (replay mode),
  compile the schedule offline — fold constant inputs into AIG constants,
  merge no-op ticks, sort transitions by domain. Profile-guided
  specialization for designs with lots of "configured once at boot" inputs.
  Composes directly with MC.5: a recording *is* a complete stimulus trace,
  so the AOT compiler can fold every input value into the kernel
  unconditionally. The resulting binary is valid only until either the
  design or the recording changes, so the lifecycle model is "compile per
  (design SHA, recording SHA) pair, cache for the test session,
  invalidate on either source changing". Acceptable cost for a 100×-replay
  regression run; not for one-off interactive sim.
- **CDC verification mode**: jitter injection on coincident edges and
  random X-injection on detected async-source paths. Reuses MC.1's
  boundary metadata and existing X-prop infrastructure. Distinct from
  static CDC checking (Spyglass, Real Intent), which is **explicitly out
  of scope** — that's a different product.

## Out of scope (explicit non-goals)

These come up adjacent and are worth being clear about:

- **Pin-level VPI / GPI fidelity.** Implementing enough VPI for unmodified
  cocotb / SystemVerilog testbenches. The surface area is enormous and
  Jacquard would be lying about delta cycles, NBA regions, `#delay`
  semantics, and X-propagation behaviour. Use transaction-level peer
  protocols (the natural extension of `input.json` over a socket) instead.
- **Metastability simulation.** No RTL simulator does this; CDC verification
  is structural/formal (Spyglass, JasperGold-CDC, Real Intent) and a
  separate product.
- **Structural CDC checking** (synchronizer recognition rules, gray-code
  analysis). Different product. MC.1's boundary metadata enables a *light*
  diagnostic but not a verification flow.
- **DUT-internal `#delay`.** Requires an event-driven kernel; destroys the
  batched dataflow that gives Jacquard its speedup. Permanently
  unsupported.
- **Async resets / latches in DUT.** Same reason. Permanently unsupported
  (already documented in `CLAUDE.md`).

## Implementation triggers

When to revisit and pull which phase off the shelf:

| Trigger | Pulls |
|---|---|
| First user workload with non-commensurable external clocks (audio, USB, DFS) | MC.2 |
| First UVM/CocoTB integration request reaches engineering scoping | MC.3 |
| User-visible CDC reporting requested | MC.1 |
| Multi-domain workload measurably bottlenecked on whole-AIG-per-tick eval | MC.1 + MC.4 |
| Regression CI total time exceeds release tolerance | MC.5 |
| Post-MC.4 measurement shows boundary-sync overhead >10 % | MC.6 speculation tier 1 (value prediction) |

## Why MC.1 and MC.2 may be worth doing standalone

The user observation in the originating discussion was that MC.1 and MC.2 are
worth carrying in a future release on their own merits, ahead of any specific
workload demand. Rationale:

- **MC.1 has standalone diagnostic value.** A "boundary report" for any
  multi-clock design — count of cross-domain combinational paths, location
  of inter-domain DFF samples — is useful to any user reviewing CDC
  structure, independent of whether the runtime ever uses the partition.
- **MC.2 lifts a real correctness limit.** The current LCM cap silently
  fails on legitimate designs (any audio-clock SoC, anything with DFS).
  Replacing precompute with a min-heap is a small, contained change that
  removes a category of "your design doesn't fit" errors.
- **Both are foundational** for the rest of the architecture. Doing them
  early means later phases pick up cleanly.

If MC.1 + MC.2 ship in isolation, they don't commit Jacquard to any of the
later phases. Each later phase remains demand-driven.

## References

- Current multi-clock infrastructure: `src/sim/cosim_metal.rs:855` and
  following (`ClockDomainFlags`, `MultiClockScheduler`).
- Per-DFF clock-domain tagging: `src/aig.rs:204` (`clock_pin2aigpins`).
- Cosim stimulus protocol: `src/sim/input_stim.rs`,
  `src/sim/models/mod.rs`.
- Existing precomputed-edge path (replay): `jacquard sim` and
  `src/sim/vcd_io.rs`.
- Adjacent committed roadmap: `docs/plans/post-phase-0-roadmap.md`.
- Synchronous-only constraint and rationale: `CLAUDE.md` "Key limitation".
