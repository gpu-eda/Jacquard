# RTL on-ramp

*Reference — the current design as it stands in the code. The why lives in the
decision records linked from each section; the not-yet-built work is under
[Implementation status](#implementation-status). Present tense means "true
today"; if a claim here is stale, the code moved and this doc didn't.*

`jacquard sim` and `jacquard cosim` accept behavioral RTL directly — no
separate build step, no external toolchain. Pass a `.v`/`.sv` file where you'd
otherwise pass a gate-level netlist, and the same command classifies the
input, synthesizes it if needed, caches the result, and simulates it on the
GPU. This is a front end onto the [simulation engine](simulation-engine.md):
the emulator core is unchanged, and only ever consumes a synthesized
structural netlist. Rationale for embedding a synthesis engine at all:
[Decision 0021](decisions/0021-behavioral-rtl-support.md).

```d2
vars: { d2-config: { pad: 16 } }
direction: down
input: "design.v / design.sv\n(or a gate-level .gv)"
classify: "Classify\n(structural parse + cell family)"
gk: "Gate-level,\nbuilt-in PDK"
gu: "Gate-level,\nunknown PDK"
behav: "Behavioral RTL"
sim: "simulate directly"
err: "error:\n--cell-descriptor <path>"
synth: "embedded Yosys\n(wasmtime, cached)"
db: "NetlistDB\n(simulation engine)"

input -> classify
classify -> gk
classify -> gu
classify -> behav
gk -> sim
gu -> err
behav -> synth -> db
sim -> db
```

## Input dispatch

`sim`/`cosim` decide what they were handed by attempting a structural parse
(`sverilogparse`) and matching the result against known cell families:

| Input | Structural parse | Action |
|---|---|---|
| Gate-level, built-in PDK (AIGPDK / SKY130 / GF180MCU) | parses, matches an `is_*_stdcell` family | simulate directly — the embedded cell-model descriptor supplies logic and timing, corner-selected via `--corner` |
| Gate-level, unknown PDK | parses, matches nothing | error: "gate-level netlist with unrecognized cells — pass `--cell-descriptor <path>`". It is already a netlist, so it is never sent to synthesis. |
| Behavioral RTL | fails (`always`/`if`/`case`/operators aren't structural) | synthesize → aigpdk netlist → simulate, via the embedded Yosys, caching the result |

The command always prints what it decided, for example `design.v: behavioral
RTL → synthesized [YoWASP Yosys, functional QoR] → <cache>`, so synthesis is
never silent. `--rtl` and `--netlist` override auto-detection when it guesses
wrong; `--emit-synth <path>` dumps the intermediate gate-level netlist for
inspection or fixture authoring. Full detection table and CLI surface:
[the accepted RTL surface](../accepted-rtl.md).

## The embedded synthesis engine

Synthesis runs as **Yosys compiled to WebAssembly, executed in-process from
Rust** (`src/synth.rs`), not a vendored native build and not a Python
subprocess. YoWASP ships Yosys as a single self-contained `yosys.wasm`, with
abc compiled in-tree into that module and called in-process — WASI has no
`exec`, so there is no separate abc process to shell out to. `src/synth.rs`
embeds [`wasmtime`](https://wasmtime.dev), loads the wasm module, preopens the
design, the `aigpdk` support files, and a temp work directory under WASI, and
runs a generated synthesis script through it. No Python interpreter and no
external toolchain: `jacquard sim design.v …` goes from RTL to waveform out of
the single `jacquard` binary. This is behind the opt-in **`synth` feature**
(`cargo build --features metal,synth`) because `wasmtime` and cranelift are
heavy to compile; a binary built without it gives an actionable error on
behavioral input instead of failing to build at all.

The SystemVerilog front end is **yosys-slang** (`read_slang`), a
near-complete SystemVerilog-2017 elaborator bundled in the pinned wasm.
`src/synth.rs` probes for `read_slang` once per wasm module (`help
read_slang`, cached for the process) and uses it when present; on an older
wasm without it, the script falls back to Yosys's built-in `read_verilog -sv`,
whose accepted subset is narrower. Which frontend ran is logged, and it
factors into the cache key below.

Two independent caches make repeat runs cheap. The compiled wasm module is
cached under `$XDG_CACHE_HOME/jacquard`, keyed by content hash, so only the
first run of a given `yosys.wasm` pays the cranelift compile — later runs
deserialize the cached module. The *synthesized netlist* is cached separately,
keyed by a hash of the design source, the generated script, the wasm bytes,
and the embedded aigpdk support files (`aigpdk_nomem.lib`, `memlib_yosys.txt`,
`gem_formal.v`), so editing any of those correctly invalidates a hit — a repeat
`sim`/`cosim` run of unchanged RTL skips synthesis entirely.

The `yosys.wasm` module itself is fetched on first use rather than bundled
into the binary. `locate_yosys_wasm` resolves it in order: an explicit
`--yosys-wasm` path, the `JACQUARD_YOSYS_WASM` environment variable, or a
pinned release fetched (sha256-verified) from the `gpu-eda/yowasp-yosys` fork
and cached under `$XDG_CACHE_HOME/jacquard/wasm/<tag>`. That fork, not a stock
YoWASP wheel, is the default source: it carries the patched abc `&origins`
path the on-ramp's provenance mapping (below) depends on, and a stock wheel
would silently drop source locations instead of failing loudly.

## Two synthesis tracks

- **On-ramp** — YoWASP Yosys, the default whenever `sim`/`cosim` are handed
  behavioral RTL. Easy: one command, nothing to install beyond the `synth`
  feature. Functional-grade QoR.
- **Performance** — bring your own synthesizer (Synopsys DC, or a native
  Yosys install) to produce a gate-level `.gv`, then point `jacquard sim` at
  it directly (the "gate-level, built-in PDK" row above). Synthesis quality is
  the primary tuner of simulation throughput, since the AIG the GPU emulates
  is only as good as the mapping. Full flow: [Synthesis flow](../synthesis-flow.md).

The two tracks share the same aigpdk libraries and memory-mapping rules
(`memlib_yosys.txt`); the on-ramp runs the same synthesis shape the
performance-path docs describe manually, just from an embedded engine instead
of a standalone Yosys or DC invocation.

## Accepted language surface

The accepted behavioral subset is whatever the embedded frontend can
synthesize to an aigpdk netlist — Jacquard delegates elaboration to Yosys
rather than implementing a Verilog front end itself. Verilog-2005 (assign,
`always @(*)`/`@(posedge/negedge)`, synchronous and async-reset flip-flops,
case/if chains, parameterized modules and `generate`, inferred memories) is
supported through Yosys's built-in reader. Through `read_slang`, the
SystemVerilog-2017 surface is substantially broader: packages, interfaces
(including modports), `always_ff`/`always_comb`/`always_latch`,
`logic`/`enum`/`struct`/`union`, and advanced `generate`. Full subset table
and known gaps: [the accepted RTL surface](../accepted-rtl.md).

Three project-specific techmaps run on top of standard synthesis, defined in
`aigpdk/gem_formal.v`:

- **`GEM_ASSERT`** — `$assert`/`$assume`/immediate `assert property`, visible
  in simulation as assertion failures.
- **`GEM_DISPLAY`** — `$display` (via Yosys `$print`); the cell is emitted by
  the on-ramp, though the display-info JSON companion the CPU side needs to
  decode format strings isn't written yet on this path.
- **`RAMGEM`** (`$__RAMGEM_SYNC_`) — inferred synchronous memories, mapped via
  `aigpdk/memlib_yosys.txt`, the same memory-synthesis step the performance
  path runs by hand.

Testbench-only constructs (`#delay`, most `initial` blocks, `$finish`,
`$stop`) have no synthesizable meaning and are dropped during elaboration.
Latches aren't supported: the emulator core is synchronous only, and a latch
inferred from RTL is a synthesis or AIG-load error.

## RTL-source provenance

The on-ramp maps to aigpdk cells via **`abc_new`**, abc9's AIGER/XAIGER path,
rather than the classic `abc -liberty` flow. XAIGER round-trips the design
through an in-memory AIGER "y" channel that carries Yosys's `(* src *)`
attributes through std-cell mapping, where the classic BLIF-based flow drops
them — BLIF has no object identity to hang provenance off. `sverilogparse`
captures those `\src` locations into `netlistdb.cell_src`, and
`AIG::aigpin_src_locations` threads them into the AIG, so `xsources` and
`--trace-signals` can report an RTL file:line instead of a flattened gate
name. A signal resolving to `binary_to_gray.sv:15` instead of
`$auto$…$1234` is the visible result. Full write-up:
[RTL-source provenance](../plans/rtl-source-provenance.md).

## Constraints

- **The emulator core does not change.** The input to the AIG/boomerang
  pipeline is always a synthesized structural netlist; behavioral elaboration
  stays Yosys's job. Jacquard does not reimplement an RTL front end, so the
  accepted surface is bounded by what the embedded Yosys accepts, not by
  anything Jacquard-specific.
- **A QoR ceiling on the easy path.** YoWASP Yosys is functional-grade —
  correct, not tuned for GPU throughput. Peak performance still wants DC or a
  native Yosys install on the performance path.
- **Concurrent-SVA synthesis is partial.** Turning `property`/`sequence`-based
  `assert property` into gate-level checkers is a separate Yosys formal-flow
  capability, independent of `read_slang`'s ability to parse SVA, and it's
  still incomplete (issues #106, #107). Immediate assertions synthesize
  correctly through the `GEM_ASSERT` mapping regardless.
- **Released binaries must be built `--features synth`.** The feature is
  opt-in at compile time because `wasmtime`/cranelift are heavy to build, but
  because behavioral input is a capability of `sim`/`cosim` themselves rather
  than a separate command, a released binary without it fails behavioral input
  with a build-without-synth message rather than silently rejecting RTL.

## Implementation status

Built and in use: the on-ramp itself (behavioral input to `sim`/`cosim`,
three-way auto-detection, the embedded wasmtime-hosted Yosys engine,
`read_slang` with fallback, both caching layers, fetch-on-first-use of the
pinned provenance wasm), and RTL-source provenance — `abc_new`/XAIGER mapping,
`\src` capture through `sverilogparse`/`netlistdb`, and its surfacing in
`xsources` and `--trace-signals`. These are the sections above without a "not
yet."

Decided but not yet built:

- **An empirical sv-tests coverage table.** The accepted-language-surface
  prose above describes the *intended* surface. The authoritative measure is
  pass/fail coverage driven through
  [sv-tests](https://github.com/SymbiFlow/sv-tests), which hasn't shipped.
  [Decision 0021](decisions/0021-behavioral-rtl-support.md).
- **Concurrent-SVA → synthesizable-checker synthesis** (#106, #107) — a Yosys
  formal-flow gap, not something `src/synth.rs` can work around.
- **Source locations in `--timing-report`.** Provenance reaches
  `--trace-signals` and `xsources`/`xroots` today; threading `\src` into the
  structured timing report is the one deferred piece of the provenance plan.
  [RTL-source provenance](../plans/rtl-source-provenance.md).

## Decisions behind this

- [Decision 0021](decisions/0021-behavioral-rtl-support.md) — behavioral RTL
  support: folding synthesis into `sim`/`cosim`, the embedded wasmtime/Yosys
  engine, the two synthesis tracks, and the RTL-source-provenance roadmap.
- [Decision 0014](decisions/0014-aig-as-simulation-ir.md) — the emulator
  model: why synthesis is a front end at all, and why Jacquard doesn't
  elaborate behavioral RTL itself.
- [Decision 0019](decisions/0019-cell-model-ir.md) — the cell-model IR: how a
  built-in PDK's descriptor supplies logic and timing together, and how
  `--corner` selects among them.
