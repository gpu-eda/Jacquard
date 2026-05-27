# ADR 0014 — AIG as simulation intermediate representation

**Status:** Accepted

## Context

Jacquard simulates gate-level RTL designs on GPUs by converting
technology-mapped netlists into an executable form.  The choice of
intermediate representation (IR) determines how easily the design maps
to GPU hardware, how much the representation compresses, and what
classes of optimisation are available at compile time.

Gate-level netlists arrive from synthesis tools (Yosys, Synopsys DC)
mapped to a variety of cell libraries: the project's own AIGPDK
library, SKY130, or GF180MCU.  Each library uses different cell names
and pin conventions; the IR must abstract over these while preserving
the combinational and sequential semantics exactly.

The GEM paper (Guo et al., "GEM: GPU-Accelerated Emulator-Inspired
RTL Simulation," DAC 2025) describes a "virtual Boolean processor"
that evaluates combinational logic as a tree of AND-with-invert
operations — directly motivating an and-inverter graph.

## Decision

### 1. Uniform AND-gate IR

All combinational logic is represented as an **and-inverter graph**
(AIG).  Every node in the combinational cone is one of:

```rust
pub enum DriverType {
    AndGate(usize, usize),    // inputs with inversion bits
    InputPort(usize),         // primary input
    InputClockFlag(usize, u8),// clock edge flag
    DFF(usize),               // sequential (D flip-flop output)
    SRAM(usize),              // memory block output
    Tie0,                     // constant zero
}
```

Only `AndGate` has combinational fan-in.  The two operands carry an
inversion bit in their LSB (`aigpin_id << 1 | invert`), giving the
full `{AND, NAND, NOR, OR}` family with a single node type.
Inverters and buffers are absorbed into the inversion bits rather than
creating separate nodes, keeping the graph compact.

This uniformity is the key property: because every combinational node
is the same `(a XOR xa) AND (b XOR xb)` operation, the boomerang
reduction tree (ADR 0015) can execute them all with a single GPU
instruction pattern — no opcode decode, no per-cell dispatch.

### 2. Conversion path: NetlistDB to AIG

The conversion is implemented in `src/aig.rs` via
`AIG::from_netlistdb_impl()`.  It handles three cell library
families:

| Library | Strategy |
|---------|----------|
| AIGPDK (native) | Cells are already AND gates, DFFs, SRAMs — direct mapping |
| SKY130 | Load Verilog behavioural models from `vendor/sky130_fd_sc_hd/`, decompose each cell into AND gates via `decompose_with_pdk()` |
| GF180MCU | Load behavioural models from `vendor/gf180mcu_fd_sc_mcu7t5v0/`, decompose similarly |
| RuntimeCellLibrary | User-supplied cell metadata (ADR 0010) for cells outside vendored PDKs |

The decomposition process for technology-specific cells:

1. **Clock tracing**: Identify sequential cells (DFFs, SRAMs), trace
   clock pins to primary inputs, create `InputClockFlag` drivers for
   posedge/negedge detection.
2. **Iterative DFS**: Walk the netlist in topological order.  For each
   unvisited output pin, recursively decompose driving cells into AND
   gates using the PDK behavioural models.  An `and_gate_cache`
   deduplicates structurally identical sub-expressions.
3. **Multi-output cells**: SKY130 cells like full adders with
   multiple outputs get special handling — shared sub-expressions are
   computed once and reused via postprocess hooks.
4. **Fanout construction**: After all pins are processed, CSR-format
   fanout arrays are built for efficient traversal.

AIG pins are guaranteed to be in topological order (pin `i` is
defined before any pin that depends on it), which the downstream
pipeline relies on for level computation and scheduling.

### 3. EndpointGroup abstraction

The AIG partitions its outputs into **endpoint groups** — the units
of work that partitions must realise:

```rust
pub enum EndpointGroup<'i> {
    PrimaryOutput(usize),     // top-level output pin
    DFF(&'i DFF),             // D flip-flop: data + clock-enable
    RAMBlock(&'i RAMBlock),   // SRAM: addr, data, enables
    SimControl(&'i SimControlNode), // $stop/$finish
    Display(&'i DisplayNode), // $display/$write
    StagedIOPin(usize),       // inter-stage boundary (from --level-split)
}
```

Each variant bundles the signals that must be evaluated together: a
DFF needs both its D input and clock-enable; an SRAM needs address,
data, and write-enable buses.  The `for_each_input()` method
enumerates all AIG pins feeding an endpoint group, which the
hypergraph partitioner (RepCut) uses to build connectivity and the
partition executor (pe.rs) uses to determine resource requirements.

This grouping is important because the boomerang reduction tree
produces results in 32-bit-aligned write-out slots.  Endpoint groups
that share a write-out slot are co-located in the hierarchy; groups
that need different clock-enable conditions (e.g., two DFFs with
different clocks driving the same data pin) generate "output
duplicates" that consume additional write-out capacity.

### 4. Why AIG over alternatives

**BDDs (Binary Decision Diagrams):** BDDs can represent Boolean
functions canonically but suffer from exponential blowup for many
practical circuits (e.g., multipliers).  The canonical form is useful
for equivalence checking but unnecessary for simulation, where we
just need to evaluate.  BDDs also have no natural mapping to the
GPU's SIMT execution model.

**Truth tables / LUTs:** Lookup tables scale exponentially with input
count.  A 6-input LUT (as in Xilinx FPGAs) covers individual cells
efficiently but doesn't compose — cascading LUTs requires separate
evaluation steps.  AIGs compose naturally: the output of one AND gate
feeds the input of the next, forming a tree that maps directly to the
boomerang hierarchy.

**Technology-mapped netlist (direct execution):** Keeping the
original cell library would require per-cell-type dispatch in the GPU
kernel — a conditional branch per node.  GPU SIMT execution penalises
warp divergence heavily; a uniform operation eliminates this entirely.
The conversion cost (one-time decomposition at compile time) is
negligible compared to the simulation runtime.

**MIG (Majority-Inverter Graph):** MIGs are a more compact
representation (3-input majority gates) but the 3-input structure
doesn't map as cleanly to binary reduction trees.  AIGs are the
industry standard for synthesis and verification tools (ABC, AIGER
format), making interop straightforward.

The AIG's key advantage is that it reduces the GPU kernel to a single
bit-parallel operation repeated across a hierarchical tree — no
opcode dispatch, no conditional branching, maximum SIMT utilisation.

## Consequences

**Enables:**

- The boomerang reduction tree (ADR 0015) works because every node is
  the same AND-with-invert operation.  A heterogeneous IR would
  require per-node dispatch and break the hierarchical reduction
  pattern.
- Technology independence: the same GPU kernel and partition executor
  handle AIGPDK, SKY130, and GF180MCU designs.  Adding a new PDK
  requires only a decomposition module, not kernel changes.
- Structural deduplication via `and_gate_cache` reduces graph size
  when multiple cells share sub-expressions.
- The inversion-bit encoding (`pin_iv = aigpin << 1 | invert`)
  eliminates inverter/buffer nodes entirely — these are free in
  hardware too, so the IR's size correlates better with actual
  simulation cost than a technology-mapped netlist would.

**Constrains:**

- **No latches or async logic.** The AIG assumes clean register
  boundaries: DFFs capture on clock edges, combinational logic is
  acyclic between registers.  Level-sensitive latches and
  combinational loops would require iterative evaluation that the
  current pipeline doesn't support (see `docs/simulation-architecture.md`
  § "Known Issues").
- **Decomposition quality matters.** A poor decomposition of a
  complex cell (e.g., a mux-heavy datapath cell) can produce a deep
  AND tree that requires more boomerang stages.  The SKY130 and
  GF180MCU decompositions are hand-tuned for the common cells; exotic
  cells from other PDKs may decompose sub-optimally.
- **No gate-delay preservation in the AIG itself.** The AIG is a
  functional (Boolean) representation.  Timing information from
  Liberty/SDF is loaded separately and overlaid onto the AIG's pin
  structure via `gate_delays` and `aigpin_cell_origins`.  This means
  the AIG construction can re-order or deduplicate nodes without
  worrying about timing — but it also means the timing model must
  reconstruct the mapping from AIG pins back to physical cells.
