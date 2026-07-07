# ADR 0021 — Behavioral RTL support via an embedded synthesis front-end

**Status:** Proposed

> **Revised 2026-07-03 (pre-ratification, still Proposed):** the entry point
> moved from a standalone **`jacquard build`** command to **folding synthesis
> into `sim`/`cosim`** — RTL is simulated with one command, no separate build
> step. The embedded-Yosys/`wasmtime` engine (Decision §2) is unchanged; only
> the *surface* changed. The original `build`-command shape is preserved in
> "Alternatives considered" for the audit trail. Rationale below.

**Relates to:** [ADR 0014](0014-aig-as-simulation-ir.md) (AIG / emulator model —
why synthesis is a front-end at all), the Python-engine work
([#161](https://github.com/gpu-eda/Jacquard/issues/161), ADR 0020 pending) that
hosts this, [ADR 0018](0018-distribution-and-installation.md) (distribution),
[`docs/synthesis-flow.md`](../synthesis-flow.md) (the manual flow this wraps),
[#162](https://github.com/gpu-eda/Jacquard/issues/162) (implementation tracking).

## Context

Jacquard is described as a "GPU-accelerated **RTL** simulator", and behavioral
RTL *is* the intended design input. But it is an **emulator** (GEM =
GPU-Emulator-inspired; [ADR 0014](0014-aig-as-simulation-ir.md)): it maps a
*synthesized* and-inverter graph onto a virtual manycore, exactly as an
FPGA-based emulator runs a synthesized bitstream, not behavioral source. So the
input to `jacquard sim` / `cosim` is a **gate-level netlist** —
structural Verilog mapped to `aigpdk` / SKY130 / GF180MCU cells — and the parser
(`sverilogparse`) is structural-only.

Behavioral RTL reaches that point through **synthesis**, which today is a
**manual, external step**: the user runs Yosys (`memory_libmap` → `aigpdk.lib`
logic synthesis) or a commercial tool per `docs/synthesis-flow.md`. This is a
genuine capability — RTL designs run fine — but it is a **UX cliff**:

- Newcomers can't tell what the tool accepts. (Repeated external feedback: "it
  wasn't clear what your netlist input language support was.")
- "Bring RTL, then run this Yosys script, then point `sim` at the output" is a
  multi-tool ceremony before the first waveform.

Two forces shape the fix:

1. **Synthesis quality drives Jacquard's speed.** `synthesis-flow.md` is
   explicit: the AIG the GPU emulates is only as good as the mapping, and a
   commercial synthesizer (DC) yields better QoR than Yosys. So synthesis is a
   real *quality knob*, not pure friction — we must not hide it in a way that
   silently caps performance.
2. **A synthesizer is embeddable now, cheaply.** [YoWASP](https://yowasp.org)
   ships Yosys as WebAssembly wheels (`yowasp-yosys`) — no system Yosys, no C++
   vendoring, cross-platform. It is **already in this workspace's `uv.lock`**
   (transitively via `amaranth[builtin-yosys]` in the mcu_soc design).

## Decision

Add an **embedded synthesis front-end** so behavioral RTL is a first-class
input, while keeping the emulator's synthesized-netlist core unchanged:

1. **`sim` / `cosim` accept behavioral RTL directly** — synthesis is a
   *transparent, cached pre-processor* inside those commands, not a separate
   user step. There is **no `jacquard build` command**. On the input path, the
   command classifies what it was handed and dispatches three ways:

   | Input | Structural parse (`sverilogparse`) | Cell family | Action |
   |---|---|---|---|
   | Gate-level, **built-in** PDK | parses | matches an `is_*_stdcell` family | **simulate directly** — the embedded descriptor supplies logic + timing (corner via `--corner`, ADR 0019) |
   | Gate-level, **unknown** PDK | parses | matches nothing | **error, actionable**: "gate-level netlist with unrecognized cells — pass `--cell-descriptor <path>`" (ADR 0019 D8). *Not synthesized* — it is already a netlist. |
   | **Behavioral** RTL | fails (`always`/`if`/`case`/operators are not structural) | n/a | **synthesize** → aigpdk → simulate, via the embedded Yosys (Decision §2), caching the result |

   The command **prints what it decided** (e.g. `design.v: behavioral RTL →
   synthesized [YoWASP Yosys, functional QoR] → <cache>`), so synthesis is
   never silent (honouring force #1 below). `--rtl` / `--netlist` override the
   auto-detection; a syntax error in a netlist that falls through to the synth
   path surfaces **both** the structural and the Yosys diagnostics. The
   gate-level artefact is dumpable for inspection/fixtures via `--emit-synth
   <path>`. RTL in, waveform out, one command — driving the *existing*
   `docs/synthesis-flow.md` scripts (`memlib_yosys.txt` → `aigpdk.lib`).

2. **Yosys as WebAssembly, executed in-process from Rust** — not a vendored
   native build, and *not* via the Python `yowasp-yosys` wrapper. YoWASP ships
   Yosys as a single self-contained `yosys.wasm` (abc is compiled **in-tree**
   into that module and called **in-process** — WASI has no `exec`), and the
   `yowasp-runtime` that runs it is a thin harness over
   [`wasmtime`](https://wasmtime.dev), which has a first-class Rust crate. So
   the synthesis engine is a **Rust component (`src/synth.rs`) embedded in the
   `jacquard` binary** that embeds `wasmtime`, loads the (bundled or
   fetch-on-first-use) `yosys.wasm`, preopens the design + `aigpdk` library
   files + a temp dir under WASI, and runs the existing synthesis script —
   caching the compiled module *and* the synthesized netlist to disk (by
   content hash) exactly as `yowasp-runtime` does. It is invoked *transparently*
   by `sim`/`cosim` on the behavioral-input path (Decision §1), behind the
   opt-in **`synth` feature** (`wasmtime`+cranelift are heavy to compile). **No
   Python interpreter and no external toolchain:** `jacquard sim design.v …` is
   RTL-to-waves from the single `jacquard` binary. This **decouples the on-ramp
   from the Python-engine packaging decision** (ADR 0020 / #161) — it needs
   neither PyO3 nor a subprocess-bundled wheel.

3. **Two synthesis tracks, kept explicit** (honouring force #1):
   - **On-ramp:** YoWASP Yosys — easy, functional; the default when `sim`/`cosim`
     are handed behavioral RTL.
   - **Performance:** bring-your-own DC (or native Yosys) → `gatelevel.gv` →
     `jacquard sim` directly (the "gate-level, built-in PDK" row above).
     Documented as the path to peak GPU speed.

4. **The emulator core does not change.** Synthesis is a transparent
   *pre-processor* inside `sim`/`cosim` that produces the same structural
   netlist users synthesize by hand today; the AIG/boomerang pipeline (ADR
   0014/0015) and the structural `sverilogparse` input are untouched — the
   behavioral path simply feeds them a just-synthesized netlist instead of a
   hand-written one. Behavioral elaboration stays Yosys's job — we do not
   reimplement an RTL front-end.

Implementation phasing lives in **[#162](https://github.com/gpu-eda/Jacquard/issues/162)**.

### Phase 2 — RTL-source provenance (roadmap, not this decision's gate)

> **Landed (2026-07).** RTL in → source-annotated waves out. The on-ramp maps via
> **`abc_new`** (abc9/aiger2 XAIGER path) so Yosys `(* src *)` origins survive
> std-cell mapping, and the WS-B ingestion (`sverilogparse` → `netlistdb.cell_src`
> → `AIG::aigpin_src_locations`) surfaces them in `xsources` + `--trace-signals`.
> Two facts below were corrected in flight: (1) the fork wasm builds via the
> **`develop-0.64` Makefile recipe (wasi-sdk 27 + yosys-slang whole-archive)**, NOT
> CMake/wasi-sdk-33 — the patched yosys fork is Makefile-only and CMake dropped
> `read_slang`. (2) The in-process `&origins` risk flagged below is **resolved** —
> origins survive the in-process WASI `abc_new` round-trip (100% `\src` on comb +
> sequential). The provenance wasm is built + released by the
> **`gpu-eda/yowasp-yosys`** fork's own CI; `jacquard` fetches the pinned release
> (A2). aigpdk-specific mapping needs `read_liberty -lib` + `hierarchy -purge_lib`
> before `abc_new` and a single mapping pass. Details:
> [`docs/plans/rtl-source-provenance.md`](../plans/rtl-source-provenance.md).

A patched toolchain carrying the [origin-shell](https://github.com/robtaylor/origin-shell)
`\src` pass-through — berkeley-abc **#487** (`vOrigins`/`&origins`) +
`robtaylor/yosys@src-retention-y-ext` — keeps RTL source locations alive through
std-cell mapping. Jacquard could then thread `\src` through `netlistdb`/AIG so
GPU-sim results **speak RTL** — `--trace-signals`, timing violations, and
X-debugging reporting source lines instead of flattened gate names.

The tractable route is **building our own `yosys.wasm` from source** rather than
depending on the stock upstream wheel. YoWASP's build (`build.sh`: **CMake +
wasi-sdk 33** via `wasi-sdk-p1.cmake`, zlib/libffi/readline/editline/tcl disabled;
recon'd 2026-07-04 — supersedes an earlier "wasi-sdk 27 / Makefile `CONFIG=wasi`"
description) compiles abc **in-tree from yosys's own bundled `abc/` submodule** —
verified against the shipped `yosys.wasm`, which embeds the abc engine (the build
path `…/yosys-src/abc/src/base/abci/…` and the full set of `&`-prefixed GIA
commands are present in the module). So a provenance build is mechanical: **fork
[YoWASP/yosys](https://codeberg.org/YoWASP/yosys)** (now on
Codeberg; the old GitHub repo was archived read-only 2026-03-11), repoint
`yosys-src` → `robtaylor/yosys@src-retention-y-ext` and yosys's bundled `abc`
submodule → `robtaylor/abc@origin-tracking-clean` (#487), and rerun `build.sh`.
Provenance then ships **self-contained in one `.wasm`** — not blocked on upstream
YoWASP adopting the patches, only on the patches themselves (abc#487 in review).

**Key open risk to spike before committing to WASM provenance:** origin-shell
validated `\src` retention through **external** abc (`ABCEXTERNAL`, temp `.aig`
round-trips). The WASM build calls abc **in-process**, a different integration
surface — the `&origins` data must survive the in-process call path, not a
temp-file hand-off. This is unproven and is the first Phase-2 task. A large build
effort overall; tracked as Phase 2 in #162, out of scope for ratifying the
on-ramp.

## Consequences

- **RTL becomes a first-class, single-command input** — the onboarding cliff is
  removed, and "what does it accept?" has a clean answer: *your RTL* (or a
  pre-synthesized netlist).
- **The accepted-RTL surface is defined and documented, not implicit.** Because
  synthesis is delegated to Yosys, the accepted behavioral subset *is* the
  embedded YoWASP Yosys frontend — and that frontend includes **yosys-slang**
  (`read_slang`), a near-complete SystemVerilog-2017 elaborator, **verified
  present in the pinned `yowasp-yosys 0.64.0.0.post1131` wasm** (`read_slang` +
  495 `slang` symbols). So SystemVerilog *language* coverage is strong
  (packages, interfaces, advanced generate, structs/enums, most of SV), not the
  narrow built-in-`read_verilog` subset. The remaining bound is **concurrent-SVA
  synthesis** — turning SVA into synthesizable checkers is a separate Yosys
  formal capability, still partial (#106/#107), independent of slang's parsing.
  On top of the frontend sit the project's techmaps (assertions → `GEM_ASSERT`,
  `$display` → `GEM_DISPLAY`, memories → `RAMGEM`), minus dropped testbench-only
  constructs. This gets a dedicated `docs/accepted-rtl.md`, whose authoritative
  form is an **empirical coverage table** driven through
  [sv-tests](https://github.com/SymbiFlow/sv-tests) (follow-up) rather than a
  hand-claimed feature list — hand-claims about a delegated frontend would
  violate the "verify, don't assert" bar.
- **A QoR ceiling on the easy path.** YoWASP Yosys is functional-grade; peak GPU
  performance still wants DC. The auto-synth path *prints its QoR tier* and the
  docs must state this, so the on-ramp isn't mistaken for the performance path.
- **A Rust `wasmtime` dependency + a `yosys.wasm` asset** — no new Python
  dependency. The `synth` feature is opt-in *at compile time* (heavy cranelift
  build), but because behavioral input is now a `sim`/`cosim` capability rather
  than a separate command, **released binaries must be built `--features synth`**
  — otherwise `sim design.v` on RTL fails with a build-without-synth message.
  A pre-synthesized netlist still simulates with the feature off. The ~39 MB
  wasm is either bundled into the binary or fetched to a cache on first use
  (open sub-decision, #162). **Resolved (2026-07, Phase 2): fetch-on-first-use.**
  `locate_yosys_wasm` (`src/synth.rs`) resolves `--yosys-wasm` → `JACQUARD_YOSYS_WASM`
  → a pinned provenance wheel downloaded (sha256-verified, cached) from a
  `gpu-eda/yowasp-yosys` release. Provenance forces this: the abc_new origins flow
  needs the fork wasm, so a bundled/stock wasm won't do (see Phase 2 below).
- **Decouples the on-ramp from ADR 0020 / #161:** because synthesis runs Yosys
  from Rust via `wasmtime`, it needs neither the PyO3 binding nor a
  subprocess-bundled Python wheel. The Python-engine packaging call can be made
  independently; the RTL on-ramp no longer waits on it.
- **Timing stays descriptor-aligned, not `.lib`-coupled.** The on-ramp does not
  reintroduce a `--liberty` runtime path: for built-in PDKs, logic *and* timing
  come from the embedded cell-model descriptor (ADR 0019 D5), corner-selected
  via `--corner`. `--liberty` / `--cell-descriptor` remain for user/custom PDKs.
- **Provenance (Phase 2)** is a large, dependency-gated follow-on, not promised
  by this ADR.

## Alternatives considered

- **Keep synthesis external (status quo), document better.** The honest interim
  and still the *performance* path — but rejected as the end state: it leaves the
  onboarding cliff that prompted this.
- **Vendor/embed native Yosys (C++).** Heavy build + distribution burden across
  macOS/Linux/arch. YoWASP's WASM wheels are the lightweight embed that makes
  this decision cheap.
- **Python-hosted front-end via the `yowasp-yosys` wrapper** (the original shape
  of this ADR). Rejected once inspection showed the wrapper is a ~1 KB shim and
  `yowasp-runtime` a ~100-line harness over `wasmtime`: hosting `build` in Python
  would force the user to have Python + pip + the wheel present (reintroducing an
  install dependency) and couple the on-ramp to the unresolved #161 packaging
  decision — all to avoid porting a small WASI harness the Rust `wasmtime` crate
  supports directly. `scripts/local_synth.py` (which drives
  `yowasp_yosys.run_yosys` for the sky130 flow) remains a useful reference for the
  synthesis-script *content*, even though the runtime is now Rust.
- **A Nix environment instead of (or alongside) YoWASP.** origin-shell is itself
  a Nix flake pinning patched yosys + abc + librelane. A Nix devshell is a viable
  alternative back-end for `jacquard build`: **native** binaries (better QoR and
  wall-clock than WASM), fully reproducible, and it carries the same patches with
  far less build engineering than a bespoke WASM toolchain — at the cost of
  requiring Nix on the user's machine. The two are complementary — YoWASP:
  zero-install, cross-platform, `pip`-native, with a WASM speed/QoR ceiling; Nix:
  native performance + reproducibility for users already in a Nix flow, and the
  natural vehicle for the Phase-2 patched (`\src`) toolchain before/without a
  WASM build. `jacquard build` should abstract the synthesis back-end so it can
  dispatch to whichever (YoWASP wheel, Nix devshell, or a plain system Yosys/DC)
  is present.
- **A standalone `jacquard build <design.v>` command** producing `gatelevel.gv`
  as an explicit user step (the original shape of this ADR; implemented on the
  first cut of PR #167). Revised away before ratification: it reinstated the very
  multi-command ceremony ("build, then point `sim` at the output") the ADR set
  out to remove, and the artefact boundary it justified turned out unneeded — the
  committed `*_synth.gv` test fixtures are each written **once** by the test that
  introduces them and never regenerated, so no standing "rebuild the fixtures"
  workflow depends on a `build` command; `--emit-synth` covers one-off fixture
  authoring. Folding synthesis into `sim`/`cosim` (Decision §1) keeps the
  one-command promise while the same `src/synth.rs` engine still backs it.
- **Elaborate behavioral RTL directly inside Jacquard** (no synthesis). Rejected:
  it contradicts the emulator model (ADR 0014) and would reimplement a Verilog
  front-end Yosys already provides — enormous scope for no architectural gain.
