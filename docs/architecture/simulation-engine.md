# Simulation engine

*Reference — the current design as it stands in the code. The why lives in the
decision records linked from each section; the not-yet-built work is under
[Implementation status](#implementation-status). Present tense means "true
today"; if a claim here is stale, the code moved and this doc didn't.*

The simulation engine compiles a gate-level netlist into a packed GPU script and
evaluates it as a branch-free Boolean processor. A design is converted once, at
the start of a run, into an and-inverter graph; the graph is partitioned to fit a
fixed GPU block geometry, and each partition becomes a stream of `u32`
instructions the kernel executes with no per-gate dispatch. This is the path
behind `jacquard sim` (static-input replay) and the substrate the
[cosim runtime](cosim-runtime.md) drives reactively.

## The pipeline

Compilation is a fixed sequence of lowering stages, each narrowing the design
toward the GPU's fixed shape:

```
Verilog netlist → NetlistDB → AIG → StagedAIG → Partitions → FlattenedScriptV1 → GPU kernel
   (structural     (cells,     (uniform  (deep-cone   (one per     (packed u32       (one block
    .gv / .v)       pins,       AND-inv   split at      GPU block,   instruction        per
                    nets, CSR)  graph)    --level-split) resource-    stream)           partition)
                                                         bounded)
```

- **NetlistDB** parses structural Verilog (`sverilogparse`) into a flattened
  database of cells, pins, and nets with CSR connectivity. Behavioural RTL is
  synthesised to this form first — see the [accepted RTL surface](../accepted-rtl.md).
- **AIG** rewrites all combinational logic to one uniform AND-with-invert node
  type, so the kernel needs no opcode decode ([Decision 0014](decisions/0014-aig-as-simulation-ir.md)).
- **StagedAIG** splits combinational cones too deep for one boomerang tree into
  sequential major stages.
- **Partitions** distribute the AIG's endpoint groups across GPU blocks under
  fixed resource limits, via hypergraph partitioning.
- **FlattenedScriptV1** packs each partition into the `u32` instruction stream the
  kernel reads directly ([Decision 0015](decisions/0015-boomerang-execution-model.md)).

Partitioning and script generation run automatically at simulation start; the
user only sets `NUM_BLOCKS` (2× the GPU's SM/CU count; 1 for Metal).

## The AIG

All combinational logic is one **and-inverter graph**. Every node in a
combinational cone is a `DriverType`:

```rust
pub enum DriverType {
    AndGate(usize, usize),     // two operands, each carrying an inversion bit
    InputPort(usize),          // primary input
    InputClockFlag(usize, u8), // clock edge (posedge / negedge) detector
    DFF(usize),                // D flip-flop output
    SRAM(usize),               // memory block output
    Tie0,                      // constant zero
}
```

Only `AndGate` has combinational fan-in. Each operand is encoded `aigpin << 1 |
invert`, so one node type covers the whole `{AND, NAND, OR, NOR}` family and
inverters/buffers fold into the inversion bit rather than becoming nodes. This
uniformity is the load-bearing property: because every combinational node is the
same `(a XOR xa) AND (b XOR xb)` operation, the boomerang tree evaluates them all
with a single instruction pattern. Rationale, and why AIG over BDDs / LUTs / MIG /
direct netlist execution: [Decision 0014](decisions/0014-aig-as-simulation-ir.md).

Construction (`src/aig.rs`, `AIG::from_netlistdb`) is technology-independent. Native
AIGPDK cells map directly; SKY130 and GF180MCU cells are decomposed into AND gates
from their vendored behavioural models; cells outside the vendored PDKs use
user-supplied metadata ([Decision 0010](decisions/0010-declarative-cell-metadata.md)).
A structural cache deduplicates identical sub-expressions, and AIG pins come out in
topological order, which the downstream level computation and scheduling rely on.

The AIG's outputs are grouped into **endpoint groups** — the units of work a
partition must realise: a primary output, a DFF (data + clock-enable), a RAM block
(address/data/enables), a `$stop`/`$finish` control node, a `$display` node, or an
inter-stage boundary pin from staging. Each group bundles the signals that must be
evaluated together; the partitioner uses the group's input set for connectivity and
the partition executor uses it for resource accounting.

## Staging deep circuits

When a design's combinational depth exceeds one 8192-wide boomerang tree,
`src/staging.rs` splits the AIG into **major stages** at user-specified level
thresholds (`--level-split 30`, or `--level-split 20,40` for two cuts). Each stage
carries its own primary inputs (the pins produced by earlier stages, or the design's
real inputs for the first), the live boundary pins it must forward, and the endpoint
groups whose depth falls within it. Major stages run sequentially — the kernel loops
over them, writing boundary values to the state buffer and re-reading them in the
next stage. Staging trades extra sequential dispatches for fitting the fixed
boomerang; without it, designs with cones deeper than ~50 levels fail partitioning
outright. Mapping details: [Decision 0015](decisions/0015-boomerang-execution-model.md).

## Partitioning

Endpoint groups are distributed across GPU blocks by **RepCut** (`src/repcut.rs`),
which builds a weighted hypergraph and cuts it with **mt-kahypar**. A hypergraph,
not a graph, because one shared AIG node reached by many endpoint groups is a single
hyperedge across all of them — cutting it costs one global-memory read shared by
every block that needs the signal, which pairwise edges cannot express. The cut
minimises Sum-of-External-Degrees, which is exactly that cross-block read count.

mt-kahypar deliberately over-partitions (≈2× the block count); `process_partitions()`
in `src/pe.rs` then greedily **merges partitions back**, scoring merge candidates by
AIG-node-bitset overlap, trying merges speculatively in parallel with
cancel-on-success, and rejecting any merge that would add boomerang stages beyond a
degradation bound. The result is 2–4× fewer partitions than the raw solution, each
validated to fit the boomerang's resource limits ([Constraints](#constraints)).

## The boomerang execution model

One GPU block (CUDA/HIP) or threadgroup (Metal) evaluates one partition by reducing
its endpoint groups' fan-in cones through a hierarchical binary tree — the
**boomerang**. The tree has `BOOMERANG_NUM_STAGES = 13` levels (`2^13 = 8192` leaf
positions); each thread owns 32 bits, so a block is `8192 / 32 = 256` threads
(`NUM_THREADS_V1`). The 13 levels map to three GPU mechanisms:

| Levels | Width | Mechanism |
|---|---|---|
| hier[0] | 8192 → 4096 | 256 threads, shared-memory reduction |
| hier[1–3] | 4096 → 512 | shared-memory reduction, barrier between levels |
| hier[4–7] | 512 → 32 | warp / SIMD shuffle (`__shfl_down_sync` / `simd_shuffle_down`), no barrier |
| hier[8–12] | 32 → 1 | bit operations within one `u32` on thread 0 |

Every position computes `(a XOR xora) AND (b XOR xorb) OR orb` — the AIG's
AND-with-invert, with `orb` all-ones turning a position into a pass-through. One
instruction pattern, zero branch divergence, maximal SIMT utilisation across all
backends. When a partition needs more than one 8192-wide tree, a **shuffle
permutation** (16-bit index pairs in the script) re-routes signals from shared
memory back into thread registers between stages. Full derivation:
[Decision 0015](decisions/0015-boomerang-execution-model.md).

## The flattened script

`src/flatten.rs` packs each partition into `FlattenedScriptV1`, a `u32` stream read
sequentially by the kernel, with four sections: a **metadata** block (256 u32 of
per-partition control fields at fixed indices, plus the write-out hook table that
maps each thread to the boomerang stage+position where it captures its output); a
**global-read permutation** that gathers each thread's input bits from the state
buffer; the **boomerang sections** (per stage, the shuffle permutation plus the
`xora`/`xorb`/`orb` AND-gate flags, with a padding slot reused to carry per-gate
gate-delay picoseconds for timing runs); and the **global write-out** that commits
results, SRAM ports, and output duplicates back to the state buffer. The metadata
index layout is the load-bearing contract between `flatten.rs` and every kernel.

One field in the global-read permutation is a **cross-backend wire format** worth
calling out: bit 31 of a read's word index flags an inter-stage intermediate (versus
previous-cycle state). One encoder sets it; four independent decoders (`flatten.rs`,
the CPU reference, the shared CUDA/HIP `kernel_v1_impl.cuh`, and `kernel_v1.metal`)
must each clear it *from the index*, never by biasing the base pointer — the pointer
trick forms an out-of-bounds address that put staged reads 2^32 words past the buffer
on ROCm, the one backend that computes the address exactly ([#203](https://github.com/gpu-eda/Jacquard/issues/203)).

## Backends and the reference model

The same script runs on four backends behind one seam. The GPU kernels —
`csrc/kernel_v1.cu` and `csrc/kernel_v1.hip.cpp` sharing `kernel_v1_impl.cuh`, and
`csrc/kernel_v1.metal` — each evaluate one partition per block. `sim` takes a
grid-wide barrier between stages, which needs cooperative launch; the
[cosim runtime](cosim-runtime.md) sidesteps that so its CUDA/HIP backends run without
it. A CPU reference kernel (`--check-with-cpu`) evaluates the same script
bit-for-bit and is the cross-backend equivalence oracle. Every backend is expected to
produce identical results on the same design; that equivalence is checked in CI.

## Selective X-propagation

By default the engine is two-state: uninitialised DFF and SRAM outputs resolve to 0,
which hides init bugs and causes false mismatches against four-state RTL simulators.
`--xprop` turns on **selective** four-state simulation. Static analysis identifies
X-source signals (uninitialised DFFs, SRAM reads); forward-cone analysis marks each
partition X-capable or X-free; only X-capable partitions — typically under ~5% after
reset — run the X-aware kernel variant and pay the ~2× storage/ALU cost, stored in
X-mask words appended to the state buffer. The rest keep the fast two-state path.
Output VCD then carries `x` values, and `--check-with-cpu` has an X-aware reference.
Design choices and the seven-phase implementation:
[Decision 0016](decisions/0016-selective-x-propagation.md) and
[`docs/selective-x-propagation.md`](../selective-x-propagation.md). The reactive
extension — undriven input pads as a third X-source, and per-edge X-mask maintenance
— belongs to the [cosim runtime](cosim-runtime.md).

## Assertions and display

`assert()` and `$display`/`$write` survive synthesis as `GEM_ASSERT` / `GEM_DISPLAY`
cells (via the `gem_formal.v` techmap over Yosys `$check`/`$print` cells). The AIG
records their bit positions in the script; after each GPU step the CPU reads those
positions and acts — an assertion fires a configurable action (log, pause, or
terminate, bounded by `max_failures`), and a display reconstructs its message from
format strings held in JSON metadata and argument bits read from the state buffer.

## Constraints

- **Synchronous, edge-triggered logic only.** Sequential state is D flip-flops
  capturing on clock edges. What is *not* modelled is a raw level-sensitive latch
  left in the logic and asynchronous *sequential* (self-timed) feedback. Three
  things often mistaken for exceptions are supported: asynchronous set/reset on
  flip-flops (it lowers to an AIG overlay), clock gating via the `CKLNQD` integrated
  clock-gating cell, and latch-based register files mapped to `$__RAMGEM_SYNC_` SRAM
  cells by memory synthesis. See the latch note in
  [Decision 0014](decisions/0014-aig-as-simulation-ir.md).
- **8191 unique inputs and 8191 unique outputs per partition** — the 8192 boomerang
  leaf/write-out slots minus Tie0. Wide buses or highly-connected cones force finer
  partitioning, which raises inter-block reads.
- **4095 intermediate pins alive per stage**, and **64 SRAM output groups per
  partition** (each SRAM consumes 4 of the 256 write-out slots). SRAM-heavy designs
  may need finer partitioning than gate count alone implies.
- **Fixed 256-thread block.** The boomerang geometry is hardcoded; there is no
  occupancy-tuning knob without redesigning the hierarchy and the script packing.

When a single endpoint cannot be mapped, `--level-split` forces stage splits.

## Implementation status

Built and in use: the full NetlistDB → AIG → StagedAIG → Partitions →
FlattenedScriptV1 → kernel pipeline; all four backends (CUDA, HIP, Metal, CPU
reference) with cross-backend equivalence in CI; RepCut hypergraph partitioning with
greedy merge-back; selective X-propagation (all seven phases, including the CPU
reference and Criterion overhead benchmarks); and assertion/display support.

Decided but not yet built:

- **Dynamic X narrowing** — periodically re-scanning the X-mask and hot-swapping a
  partition from the X-aware kernel back to the fast path once its cone is X-free.
  Distinct from the shipped static X analysis. [Decision 0016](decisions/0016-selective-x-propagation.md).
- **Per-address SRAM X** — reads currently return X until the *first* write to the
  memory; finer per-address tracking is deferred. [Decision 0016](decisions/0016-selective-x-propagation.md).

## Decisions behind this

- [Decision 0014](decisions/0014-aig-as-simulation-ir.md) — AIG as the simulation IR:
  the uniform AND-invert node, the inversion-bit encoding, the decomposition path,
  and the EndpointGroup abstraction.
- [Decision 0015](decisions/0015-boomerang-execution-model.md) — the boomerang
  execution model: the 13-level reduction tree, the GPU resource limits, RepCut
  hypergraph partitioning, and the `FlattenedScriptV1` format.
- [Decision 0016](decisions/0016-selective-x-propagation.md) — selective
  X-propagation: partition-level four-state simulation behind `--xprop`.
