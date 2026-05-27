# ADR 0015 — Boomerang execution model and GPU resource mapping

**Status:** Accepted

## Context

Once the design is converted to an AIG (ADR 0014), the combinational
logic must be mapped onto GPU hardware for parallel evaluation.  GPUs
offer massive parallelism but impose rigid constraints: fixed thread
counts per block, limited shared memory, and synchronous SIMT
execution within a warp/SIMD group.

The GEM paper (Guo et al., "GEM: GPU-Accelerated Emulator-Inspired
RTL Simulation," DAC 2025) introduces a "virtual Boolean processor"
organised as a **boomerang** hierarchical reduction tree.  This ADR
documents how the boomerang maps to GPU hardware, the resource limits
it imposes, and the partitioning and instruction-generation pipeline
that stays within those limits.

## Decision

### 1. Boomerang reduction tree

A single GPU block (CUDA/HIP) or threadgroup (Metal) executes one
**partition** of the design.  Each partition evaluates a subset of the
AIG's endpoint groups (DFFs, primary outputs, SRAMs, etc.) by
reducing their combinational fan-in cones through a hierarchical
binary tree called the **boomerang**.

The boomerang has `BOOMERANG_NUM_STAGES = 13` levels, giving a
reduction width of `2^13 = 8192` leaf positions.  Each thread in the
block handles 32 bits (one `u32`), so the block uses `8192 / 32 =
256` threads (`NUM_THREADS_V1` in `flatten.rs`).

The 13 hierarchy levels map to three GPU execution tiers:

| Levels | Width | GPU mechanism |
|--------|-------|---------------|
| hier[0] | 8192 → 4096 | 256 threads, shared memory reduction (threads 128-255 compute, 0-127 supply inputs) |
| hier[1..3] | 4096 → 512 | Shared memory reduction with barrier between levels; only threads in `[hier_width, 2×hier_width)` compute — the rest idle |
| hier[4..7] | 512 → 32 | Warp/SIMD shuffle (`__shfl_down_sync` / `simd_shuffle_down`) — no barrier needed |
| hier[8..12] | 32 → 1 | Bit-level operations within a single `u32` on thread 0 |

At each level, every position computes `(a XOR xora) AND (b XOR xorb)
OR orb` — the same AND-with-invert operation from the AIG.  When
`orb` is all-ones, the position acts as a pass-through (forwarding
input `a` unchanged).  This single instruction pattern handles AND
gates, inversions, and wiring with zero branch divergence.

Between boomerang stages (when the AIG is too deep for a single
8192-wide tree), a **shuffle permutation** redistributes results from
shared memory back to thread-local registers.  The shuffle is encoded
as 16-bit index pairs in the script, allowing arbitrary re-routing of
signals between stages.

### 2. GPU resource limits and partition constraints

The boomerang's fixed geometry imposes hard resource limits on each
partition.  These are documented in `src/pe.rs` on the `Partition`
struct:

| Resource | Limit | Derivation |
|----------|-------|------------|
| Unique inputs | 8191 | 8192 leaf positions minus Tie0.  Each input occupies a leaf slot; duplicates consume additional slots.  Global-read rounds pack multiple state words into each thread's initial register. |
| Unique outputs | 8191 | Write-out slots in the boomerang hierarchy, addressed by stage+position pairs.  Outputs include DFF data pins, primary outputs, and SRAM port signals. |
| Intermediate pins per stage | 4095 | The hier[1] level has `2^(13-1) = 4096` positions.  One position is reserved for Tie0.  Intermediates are AIG pins that are produced in one boomerang stage and consumed in the next. |
| SRAM output groups | 64 | `8192 / (32 * 4) = 64`.  Each SRAM occupies 4 write-out groups (32-bit read-data, address, write-data, write-enable).  `BOOMERANG_MAX_WRITEOUTS = 1 << (13 - 5) = 256` total write-out slots, of which SRAMs consume 4 each. |

Write-out slots are 32-bit-aligned groups within the hier[1] level.
The total write-out capacity is `BOOMERANG_MAX_WRITEOUTS = 256`.
SRAMs and "output duplicates" (same data pin driven by DFFs with
different clock enables) consume write-out slots from this budget.  A
`quick_reject()` pre-check catches obvious overflows before the
expensive full build.

When a partition exceeds these limits, `Partition::build_one()`
returns `None` and the partitioner must split the endpoint set
further.

### 3. Hypergraph partitioning with RepCut

The design's endpoint groups are distributed across GPU blocks by
**RepCut** (`src/repcut.rs`), which constructs a weighted hypergraph
and partitions it using **mt-kahypar**.

**Why a hypergraph, not a graph:**  In a standard graph, an edge
connects exactly two vertices.  But a single AIG node (an AND gate
deep in the combinational cone) may be shared by many endpoint groups
— its "edge" in the connectivity structure is a hyperedge spanning
all groups that depend on it.  Modelling this as pairwise graph edges
would lose the information that cutting this one node simultaneously
affects all connected groups.  Hypergraph partitioning minimises the
actual communication cost (shared signals that must be read from
global memory by multiple blocks).

**Why mt-kahypar:**  mt-kahypar is a state-of-the-art multilevel
hypergraph partitioner with `LargeK` support (many partitions in one
pass) and parallel execution.  The implementation uses:

- `Preset::LargeK` — optimised for k >> 2.
- `epsilon = 0.2` — 20% imbalance tolerance, giving the partitioner
  flexibility to reduce cut while keeping partitions roughly equal.
- `Objective::Soed` — Sum of External Degrees, which counts how many
  partition boundaries each hyperedge crosses.  This directly
  correlates with the number of global memory reads each block must
  perform.
- Vertex weights proportional to estimated evaluation cost (accounting
  for sub-graph size and fanout sharing).
- Hyperedge weights equal to the number of AIG nodes with that
  endpoint reachability pattern.
- Hyperedge size cap at 1000 nodes (reservoir-sampled beyond that) to
  keep partitioning tractable for signals with extreme fanout.

The hypergraph construction itself is the bottleneck for large
designs: for each AIG node, RepCut computes a bitset of which
endpoint groups it can reach via forward traversal.  Nodes with
identical reachability sets are clustered into a single hyperedge.
This is done in parallel across bitset blocks (`REPCUT_BITSET_BLOCK_SIZE
= 4096`) using Rayon.

### 4. Greedy merge-back strategy

mt-kahypar produces an initial partition assignment, but the
partition count is typically much larger than needed (set to 2x the
number of GPU blocks).  `process_partitions()` in `pe.rs` then
aggressively merges partitions:

1. **Bitset-based overlap scoring:** For each pair of partitions,
   compute the union of their AIG node bitsets.  The merge cost is
   `|union| - max(|A|, |B|)` — lower is better, indicating more
   shared sub-graph.  This is `O(num_aigpins/64)` per pair instead of
   full DFS.

2. **Speculative parallel trials:** Merge candidates are sorted by
   overlap cost.  Up to `parallel_trial_stride` merges are attempted
   in parallel using `Rayon`, with a cancel-on-success `AtomicBool`
   to abort remaining trials once a valid merge is found.  The stride
   doubles on each iteration.

3. **Quality gate:** A merged partition is rejected if it would
   increase the maximum boomerang stage count beyond
   `max_original_nstages + max_stage_degrad`.  This prevents merges
   that technically fit in resource limits but would degrade
   simulation throughput by adding extra boomerang stages.

4. **Blacklisting:** Failed merge attempts are blacklisted for that
   partition to avoid redundant retries.  Cancelled (interrupted by a
   parallel success) trials are *not* blacklisted — the merge may
   still be valid in a future iteration.

The result: 2x-4x fewer partitions than the initial hypergraph
solution, with each partition fully validated to fit within boomerang
resource limits.

### 5. FlattenedScript instruction generation

`src/flatten.rs` converts partitions into `FlattenedScriptV1` — a
packed `u32` instruction stream consumed directly by the GPU kernel.
The script encodes:

1. **Metadata section** (256 u32): Per-partition control fields at
   fixed indices, followed by the write-out hook table:

   | Index | Field | Purpose |
   |-------|-------|---------|
   | 0 | `num_stages` | Boomerang stage count |
   | 1 | `is_last_part` | Flag: last partition in the design |
   | 2 | `num_ios` | Number of write-out endpoints |
   | 3 | `io_offset` | Start offset in global state buffer |
   | 4 | `num_srams` | SRAM block count |
   | 5 | `sram_offset` | SRAM start offset |
   | 6 | `num_global_read_rounds` | Input read rounds |
   | 7 | `num_output_duplicates` | Output duplication count |
   | 8 | `is_x_capable` | X-propagation flag (ADR 0016) |
   | 9 | `xmask_state_offset` | X-mask offset (when X-capable) |
   | 128..255 | write-out hook table | Maps each thread to the boomerang stage+position where it captures its output |

   This layout is the load-bearing contract between Rust
   (`flatten.rs`) and the GPU kernel (`kernel_v1.metal`,
   `kernel_v1_impl.cuh`).

2. **Global-read permutation** (2 × `NUM_THREADS_V1` per round):
   Each thread gets an index into the global state buffer and a
   bitmask.  The thread reads one u32 from global memory and extracts
   the bits indicated by the mask using a `pext`-like loop.  Rounds
   are packed to maximise throughput (each thread accumulates up to
   32 bits across rounds).  An index high-bit flag distinguishes
   previous-cycle state from current-cycle inter-stage intermediates.

3. **Boomerang sections** (per stage, `NUM_THREADS_V1 × 20` u32):
   - 16 u32 per thread: shuffle permutation (16-bit index pairs
     selecting source bits from shared memory)
   - 4 u32 per thread: AND-gate flags (`xora`, `xorb`, `orb`) plus a
     padding slot reused for gate-delay injection (u16 picoseconds)

4. **Global write-out**: SRAM and output-duplicate permutations,
   clock-enable conditions, and data-inversion flags for committing
   results back to the state buffer.

The entire script is uploaded to device memory once and read
sequentially by the kernel.  Script reads are overlapped with
computation via double-buffering (reading the next stage's data while
computing the current stage's AND gates).

### 6. Pipeline staging for deep circuits

When a design's combinational depth exceeds the boomerang's capacity,
`src/staging.rs` splits the AIG into **major stages** at user-
specified level thresholds (`--level-split 30` or `--level-split 20,40`).

Each major stage gets its own `StagedAIG` with:

- `primary_inputs`: the AIG pins produced by previous stages (or the
  design's actual primary inputs for the first stage).
- `primary_output_pins`: live AIG pins at the split boundary that
  must be forwarded to the next stage.
- `endpoints`: the original AIG endpoint groups whose combinational
  depth falls within this stage.

Major stages execute sequentially on the GPU (the kernel loops over
them).  Between stages, intermediate values are written to the
output state buffer and re-read by the next stage's global-read
permutation (indicated by the high-bit flag in the index).

Staging trades latency (more sequential kernel dispatches) for
fitting within the 8192-wide boomerang.  Without it, designs with
>50-level combinational paths would fail partitioning entirely.

## Consequences

**Enables:**

- **Fixed, branch-free GPU kernel.** The kernel has no per-node
  dispatch — every thread executes the same AND-XOR-OR instruction at
  every boomerang level.  This maximises SIMT utilisation across
  CUDA, HIP, and Metal.
- **Deterministic shared-memory budget.** The 256-thread, 8192-bit
  boomerang uses a fixed amount of shared memory (threadgroup memory
  on Metal), independent of the design.  No dynamic allocation, no
  shared-memory pressure variation between blocks.
- **Scalable partitioning.** The hypergraph partitioner + greedy
  merge naturally adapts to designs from hundreds to millions of
  gates.  Larger designs get more partitions; the GPU kernel is the
  same.
- **Technology independence at the kernel level.** The GPU kernel
  knows nothing about AIGPDK, SKY130, or GF180MCU.  It executes
  packed u32 scripts.  All cell-library knowledge is absorbed during
  AIG construction and script generation.

**Constrains:**

- **8191-input/output ceiling per partition.** Designs with extremely
  wide buses or highly connected sub-circuits may require aggressive
  partitioning, which increases inter-partition communication (global
  memory reads).  The `--level-split` option helps by splitting deep
  cones into multiple stages, but wide cones remain fundamentally
  limited by the 8192-slot boomerang.
- **Write-out slot scarcity for SRAM-heavy designs.** Each SRAM
  consumes 4 write-out slots.  With `BOOMERANG_MAX_WRITEOUTS = 256`,
  a partition can hold at most 64 SRAMs — and fewer when output
  duplicates also need slots.  Designs with many small memories may
  need finer partitioning than their gate count alone would suggest.
- **Fixed thread count.** The 256-thread block size is hardcoded
  (`NUM_THREADS_V1`).  On GPUs where the SM/CU could benefit from
  larger blocks (e.g., occupancy tuning), there's no flexibility.
  Changing this would require redesigning the boomerang hierarchy
  depth and the bit-packing in the script format.
- **Script size grows with partition depth.** Each boomerang stage
  adds `~20 × 256 = 5120` u32 entries to the script.  Very deep
  partitions (many boomerang stages) produce large scripts that may
  pressure GPU memory bandwidth for the script reads, though
  double-buffering mitigates this.
