# Accepted Behavioral RTL Surface

`jacquard sim` and `jacquard cosim` accept behavioral Verilog / SystemVerilog
directly — no separate synthesis step, no external toolchain. This page
describes what the on-ramp synthesizes, what it drops, and where the coverage
boundary lies.

**One-line summary:** the accepted surface is whatever **YoWASP Yosys** (with
the **yosys-slang** `read_slang` frontend) can synthesize to an aigpdk netlist.
Jacquard delegates the elaboration and synthesis; it does not implement a
Verilog front-end itself.

---

## Invoking the on-ramp

```sh
# Pass behavioral RTL directly — synthesis is transparent and cached:
jacquard sim design.v in.vcd out.vcd NUM_BLOCKS

# Cosim works the same way:
jacquard cosim design.v --config sim_config.json
```

`sim`/`cosim` classify the input file automatically:

| Input | Dispatch |
|---|---|
| Behavioral RTL (structural parse fails) | auto-synthesized via embedded Yosys → simulated |
| Gate-level netlist, built-in PDK (AIGPDK / SKY130 / GF180MCU) | simulated directly, no synthesis |
| Gate-level netlist, unrecognized cells | **error** directing you to `--cell-descriptor <path>` |

Override flags:
- `--rtl` — force the synthesis path (useful if detection misclassifies a file).
- `--netlist` — force direct gate-level loading, skip detection.
- `--emit-synth <path>` — also write the intermediate synthesized netlist for
  inspection or as a fixture.

The simulator always logs its decision:
```
design.v: behavioral RTL → synthesized [YoWASP Yosys, functional QoR] → <cache>
```

---

## Providing `yosys.wasm`

The synthesis engine requires `yosys.wasm` (the YoWASP Yosys WebAssembly
module). It is located in this order:

1. **`--yosys-wasm /path/to/yosys.wasm`** — CLI flag on `sim`/`cosim` (highest
   priority; overrides the env var and discovery).
2. **`JACQUARD_YOSYS_WASM=/path/to/yosys.wasm`** — environment variable.
3. **Installed `yowasp-yosys`** Python package — discovered automatically via
   `python3 -c "import yowasp_yosys …"`. Install with:
   ```sh
   pip install yowasp-yosys          # or: uvx yowasp-yosys
   ```
4. **Fetch from release** — a planned follow-up (ADR 0021 / #162 Phase 4); not
   yet implemented. Until then, one of the three methods above is required.

> **Version caveat.** Pin to **`yowasp-yosys==0.64.0.0.post1131`** (the version in
> the project's `uv.lock`, verified to carry `read_slang`). *Newer* wheels ship a
> wasm built with the WebAssembly exception-handling proposal, which the current
> embedded `wasmtime` engine cannot load yet (`pip install yowasp-yosys` gets the
> latest and may fail with "exception refs not supported"). Loading newer modules
> is a tracked Phase-4 follow-up; until then, `pip install
> 'yowasp-yosys==0.64.0.0.post1131'`.

> Released `jacquard` binaries are built with `--features synth` and include
> the Yosys WASM runtime. Source builds must add `--features synth` explicitly:
> `cargo build -r --features metal,synth --bin jacquard`.
>
> A binary built **without** `--features synth` gives an actionable error when
> handed behavioral RTL, pointing at the synth-enabled build path.

The compiled Yosys module is cached under `$XDG_CACHE_HOME/jacquard` by
content hash — only the first run of a given wasm pays the ~20 s cranelift
compile. The synthesized netlist is also cached keyed by (design source + synth
script + wasm), so repeat `sim` runs skip synthesis entirely.

---

## SystemVerilog frontend

The embedded wasm (pinned `yowasp-yosys 0.64.0.0.post1131`) bundles
**yosys-slang** — a near-complete SystemVerilog-2017 elaborator. Jacquard
probes for `read_slang` at startup and uses it when present; it falls back
gracefully to Yosys's built-in `read_verilog -sv` for older wasm modules.

`read_slang` coverage is substantially broader than `read_verilog -sv`:
packages, interfaces, structs, enums, and most SV-2017 constructs are handled
by the slang elaborator before Yosys sees any RTL. If you observe a
"not supported" message, check whether it names `slang` or `read_verilog` — the
fallback has a narrower surface.

---

## Supported language subset

The following are synthesizable and produce correct simulation output:

**Verilog-2005 (synthesizable subset):**
- All combinational logic: `assign`, `always @(*)`, `always @(posedge/negedge)`
- Synchronous flip-flops (positive or negative edge)
- Synchronous reset DFFs (`if (rst) Q <= 0; else Q <= D;`)
- Asynchronous reset / set DFFs (async reset is supported; latches are not)
- Case and if/else chains
- Parameterized modules and `generate` blocks
- Inferred memories (mapped to `RAMGEM` by `memlib_yosys.txt` — see below)

**SystemVerilog-2017 (via yosys-slang `read_slang`):**
- Packages and package imports
- Interfaces (including modports)
- `always_ff`, `always_comb`, `always_latch`
- `logic`, `enum`, `struct`, `union` (synthesizable forms)
- Parameters with complex types; advanced `generate` (`if`, `for`, `case`)
- Casting and type conversions (synthesizable)

This is **not** the narrow subset accepted by Yosys's built-in
`read_verilog -sv`; it is the slang elaborator's synthesizable coverage, which
tracks the SystemVerilog-2017 standard closely.

---

## Project-specific mappings

The synthesis script applies three project-specific transformations on top of
standard Yosys synthesis:

| RTL construct | Result | Notes |
|---|---|---|
| `$assert`, `$assume`, immediate `assert property` | `GEM_ASSERT` cell | Visible in simulation as assertion failures. `$cover` is silently dropped. |
| `$display` (→ Yosys `$print`) | `GEM_DISPLAY` cell | The cell is emitted, but the on-ramp does not yet write the display-info JSON companion the CPU side needs to decode format strings — so `$display` text is **not surfaced through the on-ramp path** yet (tracked as a Phase-4 follow-up). |
| Inferred synchronous memories | `$__RAMGEM_SYNC_` (RAMGEM) | Mapped via `aigpdk/memlib_yosys.txt`. Asynchronous-read ports generate a warning. |

The techmap rules are in `aigpdk/gem_formal.v`. Pass
`--strip-assertions` (if available on your build) to use `chformal -remove` and
drop GEM_ASSERT cells instead, for a pure logic netlist.

---

## Known limits

**Concurrent SVA → synthesizable checker synthesis is partial.** Turning
concurrent SystemVerilog Assertions (`property` / `sequence` bound to
`assert property` with a clock) into gate-level checkers is a Yosys formal-flow
capability — independent of slang's ability to *parse* SVA. This is tracked in
issues #106 and #107. Immediate assertions (`assert (cond)` inside procedural
blocks) synthesize correctly via the `GEM_ASSERT` mapping above.

**Testbench-only constructs are dropped.** Synthesis discards constructs that
have no synthesizable meaning: `#delay` timing controls, most `initial` blocks
(other than register initialization), `$finish`, `$stop`, and similar testbench
procedural code. These are silently dropped by Yosys during elaboration, not
simulated. If a `$display` is inside an `initial` or inside a `#delay`-driven
block, it may be dropped too.

**Latches are not supported.** Jacquard's GPU emulator core is synchronous
only — see `docs/synthesis-flow.md` for the latch constraint. A latch inferred
from RTL causes a synthesis or AIG-load error.

---

## QoR note

YoWASP Yosys produces **functional-grade QoR** — correct results but not
optimized for GPU speed. Synthesis quality is the primary tuner of simulation
throughput (the AIG the GPU emulates is only as good as the mapping). For peak
performance, synthesize your design with a commercial synthesizer (DC) or a
native Yosys install, and point `jacquard sim` at the resulting gate-level
netlist directly. See `docs/synthesis-flow.md` for the performance path.

---

## Authoritative surface: empirical coverage (Phase 4)

This prose describes the *intended* surface. Because the accepted RTL surface is
whatever the embedded Yosys frontend synthesizes, the **authoritative** measure
is an empirical pass/fail coverage table driven through
[sv-tests](https://github.com/SymbiFlow/sv-tests) (or a curated subset) — a
Phase 4 follow-up tracked in `docs/plans/rtl-onramp-sim-integration.md`. Until
that table exists, hand-claimed feature lists for a delegated frontend are
inherently approximate. Report gaps as bugs; they will be tracked in the
coverage table when it ships.

---

## See also

- [Getting Started](getting-started.md) — run bundled designs to verify your build first.
- [Synthesis Flow](synthesis-flow.md) — the performance path (DC / native Yosys, peak QoR).
- [ADR 0021](adr/0021-behavioral-rtl-support.md) — the design decision behind the on-ramp.
- `docs/plans/rtl-onramp-sim-integration.md` — implementation plan and Phase 4 roadmap.
