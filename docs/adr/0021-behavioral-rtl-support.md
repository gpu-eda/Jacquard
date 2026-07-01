# ADR 0021 — Behavioral RTL support via an embedded synthesis front-end

**Status:** Proposed

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
input to `jacquard sim` / `cosim` / `map` is a **gate-level netlist** —
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

1. **`jacquard build <design.v…>`** drives Yosys through the *existing*
   `docs/synthesis-flow.md` scripts (`memlib_yosys.txt` memory synthesis →
   `aigpdk.lib` logic synthesis) to produce `gatelevel.gv`, which then feeds
   `sim` / `cosim` unchanged. RTL in, netlist out, one command.

2. **Yosys as WebAssembly, executed in-process from Rust** — not a vendored
   native build, and *not* via the Python `yowasp-yosys` wrapper. YoWASP ships
   Yosys as a single self-contained `yosys.wasm` (abc is compiled **in-tree**
   into that module and called **in-process** — WASI has no `exec`), and the
   `yowasp-runtime` that runs it is a thin harness over
   [`wasmtime`](https://wasmtime.dev), which has a first-class Rust crate. So
   `jacquard build` is a **Rust subcommand of the `jacquard` CLI** that embeds
   `wasmtime`, loads the (bundled or fetch-on-first-use) `yosys.wasm`, preopens
   the design + `aigpdk` library files + a temp dir under WASI, and runs the
   existing synthesis script — caching the compiled module to disk exactly as
   `yowasp-runtime` does. **No Python interpreter and no external toolchain:**
   `jacquard build design.v && jacquard sim …` is RTL-to-waves from the single
   `jacquard` binary. This **decouples the on-ramp from the Python-engine
   packaging decision** (ADR 0020 / #161) — `build` needs neither PyO3 nor a
   subprocess-bundled wheel.

3. **Two synthesis tracks, kept explicit** (honouring force #1):
   - **On-ramp:** YoWASP Yosys — easy, functional, the default `jacquard build`.
   - **Performance:** bring-your-own DC (or native Yosys) → `gatelevel.gv` →
     `jacquard sim` directly. Documented as the path to peak GPU speed.

4. **The emulator core does not change.** `jacquard build` is a *pre-processor*
   that produces the same structural netlist users synthesize by hand today; the
   AIG/boomerang pipeline (ADR 0014/0015) and the structural `sverilogparse`
   input are untouched. Behavioral elaboration stays Yosys's job — we do not
   reimplement an RTL front-end.

Implementation phasing lives in **[#162](https://github.com/gpu-eda/Jacquard/issues/162)**.

### Phase 2 — RTL-source provenance (roadmap, not this decision's gate)

A patched toolchain carrying the [origin-shell](https://github.com/robtaylor/origin-shell)
`\src` pass-through — berkeley-abc **#487** (`vOrigins`/`&origins`) +
`robtaylor/yosys@src-retention-y-ext` — keeps RTL source locations alive through
std-cell mapping. Jacquard could then thread `\src` through `netlistdb`/AIG so
GPU-sim results **speak RTL** — `--trace-signals`, timing violations, and
X-debugging reporting source lines instead of flattened gate names.

The tractable route is **building our own `yosys.wasm` from source** rather than
depending on the stock upstream wheel. YoWASP's build (`build.sh`: wasi-sdk 27 →
flex → yosys `CONFIG=wasi` + LTO) compiles abc **in-tree from yosys's own bundled
`abc/` submodule** — verified against the shipped `yosys.wasm`, which embeds the
abc engine (the build path `…/yosys-src/abc/src/base/abci/…` and the full set of
`&`-prefixed GIA commands are present in the module). So a provenance build is
mechanical: **fork [YoWASP/yosys](https://codeberg.org/YoWASP/yosys)** (now on
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
- **A QoR ceiling on the easy path.** YoWASP Yosys is functional-grade; peak GPU
  performance still wants DC. The docs must state this so `jacquard build` isn't
  mistaken for the performance path.
- **A Rust `wasmtime` dependency + a `yosys.wasm` asset** — no new Python
  dependency, and the on-ramp is optional (the `sim`/`cosim`/`map` binary keeps
  working on a pre-synthesized netlist with `build` unused). The ~39 MB wasm is
  either bundled into the binary or fetched to a cache on first `build` (open
  sub-decision, #162).
- **Decouples the on-ramp from ADR 0020 / #161:** because `build` runs Yosys from
  Rust via `wasmtime`, it needs neither the PyO3 binding nor a subprocess-bundled
  Python wheel. The Python-engine packaging call can be made independently; the
  RTL on-ramp no longer waits on it.
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
- **Elaborate behavioral RTL directly inside Jacquard** (no synthesis). Rejected:
  it contradicts the emulator model (ADR 0014) and would reimplement a Verilog
  front-end Yosys already provides — enormous scope for no architectural gain.
