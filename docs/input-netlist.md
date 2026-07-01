# Input: netlist language & RTL

## What Jacquard reads

Jacquard is a gate-level **emulator** — it maps a *synthesized* and-inverter
graph onto a virtual manycore GPU processor (see
[ADR 0014](adr/0014-aig-as-simulation-ir.md)). So the direct input to
`jacquard sim` / `cosim` / `map` is a **gate-level Verilog netlist**: structural
Verilog whose leaf cells are `aigpdk`, SKY130, or GF180MCU standard cells.

**Behavioral RTL is the intended design input — via a synthesis step.** You
bring your RTL, synthesize it to a gate-level netlist (memory mapping +
logic synthesis to `aigpdk.lib` cells), and Jacquard emulates the result at
5–40×. Synthesis is currently a **separate, documented step** — see
[Synthesis Flow](synthesis-flow.md) — because synthesis *quality* directly sets
Jacquard's performance, so it's a deliberate knob (open-source Yosys works;
a commercial tool like DC gives better QoR).

> **Roadmap:** an integrated `jacquard build design.v` on-ramp (Yosys via
> YoWASP, no manual synthesis) is planned — see
> [ADR 0021](adr/0021-behavioral-rtl-support.md) and
> [#162](https://github.com/gpu-eda/Jacquard/issues/162). Until then, synthesize
> first, then `sim`.

Behavioral constructs (`always`, `if`/`case`, `reg`, parameters, `generate`,
`function`/`task`, `#delay`) are **not read by Jacquard directly** — the
synthesizer elaborates them away. If you feed raw behavioral RTL to `sim`, it
will fail to parse; run it through synthesis first.

## Supported netlist syntax

The netlist parser (`sverilogparse`, consumed by `netlistdb`) accepts the
structural subset a synthesizer emits:

**Structure**
- Multiple `module … endmodule` per file; `//` and `/* */` comments; `(* … *)`
  attributes (parsed and ignored).
- Both port-list styles: non-ANSI (`module m(a, b);` with body `input`/`output`
  decls) and ANSI (`module m(input [7:0] a, output b);`). A `wire`/`reg`
  net-type keyword in an ANSI header is accepted and ignored.

**Declarations**
- `input` / `output` / `inout` / `wire`, scalar or bus (`[hi:lo]`), with
  comma-separated names (`wire a, b, c;`).

**Cell instantiations**
- `CELL_TYPE inst (.pin(expr), …);` — **named-port connections only**. An empty
  `.pin()` (unconnected) is accepted and dropped.

**Assigns**
- `assign lhs = rhs;`, both sides full wire expressions.

**Wire expressions** (in assigns, port connections, and `.name(expr)` hookups)

| Form | Example |
|------|---------|
| Identifier (scalar or whole bus) | `w` |
| Bit-select | `w[3]` |
| Part-select / slice (`[hi:lo]` or `[lo:hi]`) | `w[7:0]` |
| Concatenation | `{a, b[3:1], 4'b0101}` |
| Sized literal (x/z allowed for bin/oct/hex) | `4'b01xz`, `8'hFF`, `10'd42` |
| Unary NOT | `~w` |
| Parenthesised grouping | `(w)` |

**Identifiers** — standard (`[A-Za-z_][A-Za-z0-9_$]*`) and escaped
(`\name-until-whitespace `).

## Not supported in the netlist

Beyond the behavioral constructs above (which belong in *pre*-synthesis RTL),
the parser does **not** accept:

- **Positional port connections** — `CELL inst(a, b, c)`; use `.pin(expr)`.
- **Parameters** — `parameter` / `localparam`, or `#(…)` overrides on instances.
- **Preprocessor** — `` `define `` / `` `ifdef `` (no preprocessor).
- **Net types** beyond `input`/`output`/`inout`/`wire` — no `supply0/1`, `tri`,
  `wand`/`wor`, `trireg`.
- **Multi-dimensional / memory arrays** — one `[hi:lo]` range per declaration.
- **Bare unsized decimal literals** — write `1'b1`, not `1`.
- **Replication** — `{4{a}}`.
- **`~` inside a concatenation** — `{~a, b}` (use an explicit inverter cell).
- **Operators** — `&`, `|`, `?:`, arithmetic; and base-10 literals with x/z
  (`8'dx`).

## Design-level constraints (`netlistdb`)

- **Single top module** — auto-detected as the module no other instantiates; pass
  `--top-module <name>` if ambiguous. Cyclic instantiation is rejected.
- **Hierarchy is flattened** — multi-level module hierarchy is supported and
  flattened at load.
- **Leaf-cell pin directions come from the cell library** (`aigpdk` / SKY130 /
  GF180MCU); an unknown cell's pins default to `Unknown` with a warning.
- **`inout` on the design boundary** is parsed but modelled as `Unknown`
  (not fully supported); prefer split `input`/`output` where possible.
- **Edge-triggered flops only** — a raw `LATCH` cell is rejected (async
  set/reset is fine). Clock gating uses `CKLNQD`; latch/register-file memory maps
  to RAM through the memory-synthesis step (see [Synthesis Flow](synthesis-flow.md)).

## SystemVerilog & assertions (SVA)

- **Immediate assertions** are already lowered through synthesis today, as
  `GEM_ASSERT` cells (see the assertion handling in `src/aigpdk.rs`).
- **Broader SVA** (SystemVerilog Assertions) is **planned** — see
  [Testbench Interop § Roadmap](interop.md). The concrete near-term slice is the
  **X-barrier `$isunknown` assertion** work: a spec-file-driven `!$isunknown`
  check against the X-mask ([#106](https://github.com/gpu-eda/Jacquard/issues/106))
  and lowering an `$isunknown` SVA subset from RTL to that spec
  ([#107](https://github.com/gpu-eda/Jacquard/issues/107)), building on selective
  X-propagation ([ADR 0016](adr/0016-selective-x-propagation.md)).

## See also

- [Synthesis Flow](synthesis-flow.md) — RTL → gate-level netlist (the step above).
- [Testbench Interop](interop.md) — UVM / cocotb / SVA, and record-and-replay.
- [ADR 0014](adr/0014-aig-as-simulation-ir.md) — why the input is a synthesized AIG.
- [ADR 0021](adr/0021-behavioral-rtl-support.md) — the planned integrated RTL on-ramp.
