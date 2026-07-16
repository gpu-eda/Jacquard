# Plan — RTL-source provenance (ADR 0021 Phase 2)

> **2026-07-15 — shipped in v0.3.0.** Behavioral RTL → `sim` → a signal resolves
> to its RTL line, in a released binary. `src/synth.rs` maps via `abc_new` and
> emits `\src`; the provenance fork lives at **`gpu-eda/yowasp-yosys`**, whose CI
> builds + validates + **releases** the wheel, and `jacquard` fetches that pinned
> release on first use (no PAT, no artifact expiry). aigpdk mapping needed two
> fixes the sky130 A0 didn't surface: `read_liberty -lib` + `hierarchy -purge_lib`
> before a single `abc_new` pass. Verified against third-party RTL (PULP
> common-cells via core-v-mcu): the emitted netlist carries
> `(* src = "binary_to_gray.sv:15.8" *)`.

**Status:** **Shipped (v0.3.0)** with one deferred item and one known toolchain
bug. Per-section ✅ markers below remain authoritative for detail.

- **WS-A — done.** The forked provenance wasm builds and carries origins through
  the in-process WASI `abc_new`/`aiger2` XAIGER-`"y"` path (100% `\src` coverage
  on comb + seq2). A1 (harden/pin) + A2 (distribute) + the `src/synth.rs` →
  `abc_new` integration all landed (#176).
- **WS-B — B0–B3 done** (`sverilogparse` capture → `netlistdb.cell_src` →
  `AIG::aigpin_src_locations`, surfaced in `xsources` and `--trace-signals`).
  **Deferred: B3's timing-report surface** — src locations in `--timing-report`.
  That's the one piece of the original plan not shipped.
- **Known bug in the toolchain this plan adopted:** `abc_new` hard-traps (wasm
  `unreachable`) on a design that reaches it with **zero primary I/O** — bisected;
  neither the constant clock nor emptiness is the trigger, one port of either
  direction avoids it, and the failure is inside the `ABC9` sub-pass. Whether
  `&origins` causes it or it's stock abc9 is **open**; the A/B is blocked because
  a stock YoWASP wheel won't load under our `wasmtime` (built with wasm exception
  handling, which we don't enable). Tracking:
  [robtaylor/origin-shell#1](https://github.com/robtaylor/origin-shell/issues/1)
  upstream, [#211](https://github.com/gpu-eda/Jacquard/issues/211) here (whose
  reporting half is fixed — a trap now names the pass instead of printing 27 wasm
  frames). Low impact for real designs: a portless module is testbench-shaped.

**ADRs:** [0021](../adr/0021-behavioral-rtl-support.md) Phase 2 (the roadmap this
realises), [0014](../adr/0014-aig-as-simulation-ir.md) (AIG core the provenance
rides through), [0018](../adr/0018-distribution-and-installation.md) (wasm
distribution).

**Predecessors:** ADR 0021 Phase 1 (the on-ramp — `sim`/`cosim` synthesize
behavioral RTL via embedded YoWASP Yosys). This plan makes the *results* of that
simulation speak RTL source locations instead of flattened gate names.

**Tracking:** [#162](https://github.com/gpu-eda/Jacquard/issues/162) Phase 2.

---

## Goal

Thread **`\src`** (RTL source `file:line`, Yosys's source-provenance attribute)
from synthesis all the way to Jacquard's user-facing outputs, so that
`--trace-signals`, timing-violation reports, and X-debugging (`xsources`/`xroots`)
report **RTL source locations** rather than post-synthesis flattened gate names.
"Why is `spiflash.ctrl.\$auto$…$1234` X?" becomes "`spiflash.v:88`".

## Why it's two independent problems

1. **The toolchain must *emit* `\src`.** Stock YoWASP Yosys drops source
   provenance through std-cell mapping (abc). Carrying it needs a patched
   toolchain — the [origin-shell](https://github.com/robtaylor/origin-shell)
   `\src` pass-through: berkeley-abc **#487** (`vOrigins`/`&origins`) +
   `robtaylor/yosys@src-retention-y-ext`.
2. **Jacquard must *ingest* `\src`.** Even with a perfect provenance netlist,
   Jacquard throws it away today: `sverilogparse` **discards all attributes**
   (`vendor/eda-infra-rs/sverilogparse/src/sverilognom.rs:44` — *"we regard
   attributes as comments"*), and `netlistdb` has no source-location field.

These are **independent** and should be de-risked **in parallel**: WS-B (ingestion)
can be prototyped end-to-end against a **hand-annotated** `(* src=… *)` netlist
before the real toolchain (WS-A) exists. Only invest in the heavy wasm build
(A1) once *both* the A0 spike passes and WS-B is proven.

---

## WS-A — Provenance-carrying toolchain

> **Reframed after reading origin-shell (2026-07-04).** The gate is **not**
> "does `\src` survive *in-process* abc" — origins ride the **XAIGER `"y"`
> channel** (an in-memory AIGER round-trip via the `aiger2` reader/writer, keyed
> by object id), so external-vs-in-process abc is irrelevant to the channel.
> origin-shell's real lesson: **provenance cannot survive the classic
> `abc`/BLIF path at all** (BLIF has no object identity), so the std-cell flow
> must move to `abc9`/`abc_new`. Data path (origin-shell POC):
> `abc_new: write_xaiger2 → ABC (&read; &origins; &dch -f; &nf) → read_xaiger2`.
>
> **This means two things Jacquard's own `synth.rs` must change**, independent of
> the patched wasm:
> - `src/synth.rs` maps with **classic `abc -liberty`** (lines 241/244) → must
>   become the **`abc_new`** flow (`&dch -f; &nf`, `scratchpad -set
>   abc9.origins_max N`) to carry origins.
> - It writes with **`write_verilog -noattr`** (line 247) — `-noattr` **strips
>   `\src`**. Must drop it (attribute-selective if needed).
>
> **QoR caveat:** `abc9` std-cell mapping is "the road less travelled" upstream
> (`YosysHQ/yosys#5679` removed `abc9 -liberty`; the yosys fork carries the
> `aiger2` std-cell path). Bare `&nf` is 9–22% worse area than classic `abc`;
> `&dch -f; &nf` recovers to parity. So the flow change carries a **QoR
> validation obligation**, not just a wiring change.

### A′ — De-risk the `abc_new` flow on the **stock** wasm first (no patched toolchain)

`abc_new`/`&nf` exists in the **stock** `yosys.wasm` we already ship (just without
the `&origins` patch). So the flow migration + its QoR risk can be proven **before**
any fork/build work, moving the "road less travelled" risk off the patched-wasm
critical path.

- In `src/synth.rs`, switch the two `abc -liberty aigpdk_nomem.lib` passes to the
  `abc_new` `&dch -f; &nf` flow against `aigpdk_nomem.lib`; keep `-noattr` for now
  (no origins to preserve yet).
- **Exit:** the on-ramp still produces a correct aigpdk netlist via `abc_new`, and
  QoR (cell count / area from `stat`) is at parity with the classic-`abc` baseline
  on the counter/assert/mem designs.

> **A′ spike result (2026-07-05, stock `yowasp-yosys 0.64`) — the early signal
> fired.** Ran `abc_new -script +&dch,-f;&nf -liberty aigpdk_nomem.lib` on a small
> design (scratchpad `/tmp/claude/aprime`):
> - **Combinational-only** (adder) → **works** (maps to `$_NAND_`/`$_XNOR_`/…).
> - **Sequential** (a `posedge` flop) → **FAILS**: `ERROR: Bad connection
>   $auto$ff.cc:337:slice$150/D ~ \d [1]` (abc_new marked "experimental").
> - classic `abc` baseline on the same design → works, **33 cells** (23 AND2 + 4
>   DFF + 6 INV).
>
> The `ff.cc` error is precisely the flop-handling that
> `robtaylor/yosys@src-retention-y-ext` fixes (origin-shell lists "the
> `ff.cc`/`memory_map`/`mem.cc` fixes"). **So A′ cannot be fully de-risked on the
> stock wasm** — the yosys fork is required even to make `abc_new` *function* on
> sequential logic, not just to carry origins. Consequence: **A′ folds into A0** —
> the first real test is building the forked wasm and running the `abc_new` flow
> there (getting flow-correctness + QoR + origins in one shot), since stock can
> only validate the purely-combinational sub-case.

### A0 — GATING: build the forked wasm, check `\src` survives

With A′ proving the `abc_new` flow works, A0 adds the **origins**: build the
patched wasm and confirm `\src` rides the XAIGER `"y"` channel through mapping.
The spike **is** the build (the wasm is the A1 artifact — no throwaway work).

**Build inputs — ✅ ALL WIRED + VERIFIED 2026-07-06** (the two repoints below are
done; recon was against Codeberg `YoWASP/yosys`, 2026-07-04). Chain:
```
robtaylor/yowasp-yosys @ yowasp-yosys-integration   (NEW; mirror of Codeberg, base develop-0.64)
  └─ yosys-src → robtaylor/yosys@src-retention-y-ext  (bcc5698)
       └─ abc  → robtaylor/abc@origin-tracking-clean  (2daf32f2, #487)   ← abc is nested in yosys-src
       └─ yosys-slang-src → povik/yosys-slang          (SV frontend, upstream)
```
1. ✅ **`yowasp` overlay:** created `robtaylor/yowasp-yosys` (full mirror; default
   `develop` = clean mirror, `yowasp-yosys-integration` carries the repoint), repointed
   `yosys-src` → `robtaylor/yosys@src-retention-y-ext`.
2. ✅ **Inside the yosys fork:** repointed `robtaylor/yosys@src-retention-y-ext`'s `abc`
   submodule `YosysHQ/abc` → `robtaylor/abc@origin-tracking-clean` (commit `bcc5698`).
   Without this the build would have yosys-side `\src` retention but stock abc, and
   provenance would die at mapping.

**Build mechanics — ⚠️ CORRECTED 2026-07-06 (supersedes the "CMake + wasi-sdk 33"
recon; that was `develop` HEAD, the WRONG base for our fork).** Two hard constraints
force the **`develop-0.64` Makefile recipe**, not `develop`'s CMake:
1. `robtaylor/yosys@src-retention-y-ext` is **Makefile-only (no `CMakeLists.txt`)** —
   `develop`'s `cmake -S yosys-src` cannot build the patched fork at all.
2. `develop`'s CMake migration (`61073ed "CI: switch to CMake based builds"`)
   **dropped yosys-slang** — its wasm has **no `read_slang`**, the SV frontend the
   on-ramp depends on. `develop` also moved to wasi-sdk 33.

So base the overlay's integration branch on **`develop-0.64`**: `build.sh` is the
**Makefile** flow (`make -f yosys-src/Makefile CONFIG=wasi`, `-flto`, `-Wl,-z,stack-size=8M`)
+ **wasi-sdk 27** + **flex-2.6.4 built from source** + **yosys-slang built via cmake
and whole-archive-linked** (`libyosys-slang.a`) — the exact recipe that built the
pinned `yowasp-yosys 0.64` the on-ramp uses. It hardcodes **x86_64-linux** wasi-sdk →
**Linux/Docker/CI only** (done: the A0 CI runs `ubuntu-latest`).

**A0 CI (authored 2026-07-06):** `robtaylor/yowasp-yosys/.github/workflows/provenance-wasm.yml`
— `build` (compile → WASI, link slang, smoke `read_slang`/`abc_new`, upload wasm+wheel)
+ `provenance-check` (origin-shell's `abc_new` origins flow on `comb`+`seq2` vs sky130,
`\src` coverage; seq2@0% fails). **Residual risk CI will surface:** develop-0.64's
yosys-slang pin `4e53d772` targets yosys 0.64 (Apr); our fork's yosys is ~Jun 2026, so
slang may need a newer pin to compile.

- Repoint (steps 1–2 above), run `build.sh`, and run the A′ `abc_new` synth flow
  **with `origins_max` set and `-noattr` dropped** on a small multi-module design;
  measure the **`\src` coverage** — the % of mapped cells carrying a `src`
  attribute. Crib origin-shell's `test/src_coverage.sh` harness (loads `write_json`
  output, counts `'src' in cell['attributes']` over mapped cells) — reuse it near
  verbatim.
- **Go/no-go:** high `\src` coverage on mapped cells → **you already hold the
  provenance wasm** (A1 done); proceed to harden (A1) + distribute (A2). Low/zero →
  the `aiger2` `"y"` channel or abc `&origins` isn't functioning in the wasm build;
  diagnose before investing further.

> **✅ A0 GO (2026-07-06, CI run `28779240456`).** The provenance wasm builds and
> carries origins: `comb 4/4 = 100%`, **`seq2 88/88 = 100%`** `\src` coverage on
> mapped sky130 cells. Origins survive the **in-process WASI `abc_new`** round-trip
> (including sequential logic) — the ADR's key open risk is resolved. Getting here
> also required rebasing `robtaylor/abc@origin-tracking-clean` onto current
> `berkeley-abc/master` (it was 97 commits behind, missing the `#ifdef __wasm` guards
> around abc's `system()` calls → `wasm-ld: undefined symbol: system`). CI:
> `robtaylor/yowasp-yosys/.github/workflows/provenance-wasm.yml`.
- **Fallbacks if it fails:** (a) debug the `aiger2`/`&origins` path in the wasm
  build (the channel is XAIGER round-trip, not exec, so the failure is in the
  reader/writer or the patch, not "in-process abc" per se); (b) **Nix native
  toolchain** — origin-shell is already a Nix flake with the *validated* flow, so a
  native back-end for `jacquard sim --synth-backend nix` is the lowest-risk way to
  ship provenance if the wasm path proves stubborn; (c) native-only provenance,
  on-ramp wasm stays provenance-free.

### A1 — Harden & pin the provenance build

The A0 build, productionized. **Thin harness fork, not our own build recipe:**
YoWASP/yosys is just a build harness, so the overlay's whole delta is the two
submodule repoints — trivial to rebase when upstream moves, and it beats
hand-rolling a WASI/LTO build (where subtle divergence bites hardest). The durable
maintenance is keeping the *existing* `yosys`/`abc` patch branches rebased on
upstream — unavoidable in any approach, already owned. Per
[`~/.claude/FORKED_DEPS_WORKFLOW.md`], carry each of the three on an
`-integration` branch.

**Pinning — track our branches, don't freeze SHAs.** These are *our* actively
developed branches (we'll tweak the `\src` patches, and want to fold in upstream
review of abc#487), so pinning them to frozen SHAs would fight normal development.
Instead:
- **Track by branch**, not SHA, for the three forks we own (`yosys`/`abc`/`yowasp`
  overlay) — new patches and upstream-review changes flow through automatically;
  when a patch merges upstream, retarget the branch to upstream.
- **Pin only genuinely-third-party build inputs** we don't control and need
  reproducible: **wasi-sdk version**, flex, and the rest of `build.sh`'s toolchain.
- **Stamp provenance into each built artifact**, not the source: when CI builds a
  released wasm, record the exact `yosys`/`abc`/`yowasp` SHAs (release notes /
  asset metadata). That gives *release-level* reproducibility — any shipped wasm
  is recreatable — without freezing the dev branches. Develop freely; ship
  traceably.

**Exit strategy:** upstream the patches (abc#487 → yosys → a YoWASP build knob) to
shrink the forks toward zero.

- **Exit:** a pinned, CI-reproducible wasm that runs the existing `synth_script`
  and emits `(* src *)` on mapped cells; byte-diff vs the stock wasm shows only
  provenance additions.
- **Escape hatch (own-recipe):** only if A0 shows `build.sh` is
  gnarly/undermaintained enough that rebasing the overlay is painful — then
  reimplement a minimal pinned WASI build in-repo. Default is the thin fork.

### A2 — Distribute the provenance wasm

Publish as a Jacquard release asset (shares the Phase-4 fetch-from-release
mechanism). Decide: is provenance the *default* on-ramp wasm, or an opt-in
(`--yosys-wasm`/a `provenance` flag) larger asset? Recommend opt-in first.

---

## WS-B — Jacquard `\src` ingestion (startable **now** with a synthetic netlist)

### B0 — Capture attributes in `sverilogparse`

Stop discarding `(* … *)` (`sverilognom.rs:44`). Parse `src` (and keep the door
open for other attrs) and attach to the cell/wire in the `SVerilog` AST.
`vendor/eda-infra-rs` is a **vendored submodule** → fork + integration branch per
[`~/.claude/FORKED_DEPS_WORKFLOW.md`](../../).

- **Exit:** a `(* src="f.v:12" *)`-annotated netlist round-trips through
  `SVerilog::parse_*` with the attribute retrievable per cell.

> **✅ B0 DONE (2026-07-06).** `gpu-eda/eda-infra-rs@jacquard-integration` `a0772f4`;
> `SVerilogCell` gains `src: Option<CompactString>`. `skip_whitespace_and_comment`
> no longer treats `(* ... *)` as whitespace; `leading_attributes` captures/skips
> at the 3 grammar leading edges (module / port / module-body item). Proven by
> `sverilogparse` `test_src_attribute` (+ `tests/attributes.v`): parse + round-trip.
> **Caveat:** attributes are now only accepted at those leading edges (narrows vs
> the old "ignored anywhere" — fine for structural netlists).

### B1 — Carry source location through `netlistdb`

Add an optional per-cell (and where meaningful, per-net) `source_loc` on
`NetlistDB`, populated from B0. Keep it `Option` — most cells post-synthesis may
have 0 provenance.

- **Exit:** `NetlistDB::from_sverilog_file` exposes `\src` for annotated cells;
  zero overhead / behaviour change when absent.

> **✅ B1 DONE (2026-07-06).** `gpu-eda/eda-infra-rs@jacquard-integration` `4f7f0ec`;
> `NetlistDB.cell_src: Vec<Option<CompactString>>` parallel to `celltypes`/`cellnames`,
> populated in `insert_cell` from `SVerilogCell.src` through flattening. `None` for
> top cell + synthesised inverters + un-annotated cells. Proven by
> `netlistdb/tests/provenance.{rs,v}`.

### B2 — Preserve provenance through AIG / staging / flatten

Thread a mapping from AIG nodes / endpoint groups back to `\src` so a
sim-visible signal can be traced to a source line. Provenance is **lossy** by
nature (abc merges/splits nodes): design for **0, 1, or many** source locations
per signal; never assert exactly one.

- **Exit:** given an annotated netlist, a chosen output/endpoint resolves to its
  source location(s) through the built `FlattenedScript`.

> **✅ B2 DONE (2026-07-06, Jacquard `68cc938b`).** `AIG::aigpin_src_locations`
> composes the AIG's existing per-pin cell-origin map (`aigpin_cell_origins`, built
> for SDF back-annotation) with `netlistdb.cell_src` (B1) — **no new state threaded
> through the AIG builder.** Returns 0/1/many de-duplicated locations; a primary
> output resolves directly. The resolver is AIG-level (the AIG is retained alongside
> the FlattenedScript). Proven by `aig::path_mapping_tests` + `prov_annotated.v`.

### B3 — Surface `\src` in the user-facing outputs

Add source locations to the three consumers, gated on availability (fall back to
today's hierarchical gate name when absent):
- `--trace-signals` (`docs/signal-tracing.md`) — annotate traced signals.
- Timing-violation reports (`docs/timing-violations.md`).
- X-debugging `xsources` / `xroots` (`docs/x-debugging.md`) — the highest-value
  target: report the RTL line of an X-source, not the flattened gate.

- **Exit:** an end-to-end run on a synthetic annotated netlist prints source
  locations in all three; a design with no provenance is unchanged.

> **⏳ B3 IN PROGRESS (2026-07-06) — 2 of 3 consumers done.**
> - ✅ `xsources` (`ec7b648f`): optional `src` on the `XSource` record from
>   `cell_src[cell_id]`; schema `1.0`→`1.1`. (`xprop_demo_synth.gv` already carried
>   `(* src *)` → provenance flows through the real pipeline.)
> - ✅ `--trace-signals` (`ca55c786`): logs each traced net's RTL `src` via the B2
>   resolver at registration.
> - ⏳ **timing-violation reports** — deferred (largest; governed schema). Path:
>   `word_id → cell_id` via `dff_constraints`; add `src` to `DffSiteName` →
>   `ViolationRecord`. A word packs many DFFs → 0/1/many. **ADR 0008 permits
>   additive** → bump `SCHEMA_VERSION` `1.2.0`→`1.3.0` + ADR note.

---

## Sequencing

```
A′ abc_new on STOCK wasm (flow+QoR) ─► A0 patched wasm + \src coverage ─(go)─► A1 harden/pin ─► A2 distribute ─┐
                                                                                                           ├─► integrate: on-ramp emits + surfaces \src
B0 parser ─► B1 netlistdb ─► B2 AIG ─► B3 outputs ─────────────────────────────────────────────────────────┘   (validated on synthetic netlist first)
```

- **Parallel:** the whole A-track and B0–B3 have no dependency; B is provable
  against a hand-written annotated netlist, and A′ against the stock wasm.
- **A′ first within WS-A:** proves the `abc_new` flow + QoR on the stock wasm
  before any fork/build, moving the "road less travelled" QoR risk off the
  patched-build critical path.
- **Barrier:** full end-to-end (RTL → provenance wasm → surfaced source lines)
  needs A1+A2 *and* B0–B3.
- **Kill-switch:** if A0 fails, WS-B still delivers value for any *externally*
  produced provenance netlist (DC/native Yosys with origins); the on-ramp just
  doesn't auto-generate it until a fallback toolchain lands.

## Risks / open questions

- **`abc9`/`aiger2` `"y"` channel in the wasm build** — the gating unknown (A0):
  does the XAIGER origins round-trip function in the WASI-built yosys+abc?
- **QoR of the `abc_new` std-cell flow** — abc9 std-cell mapping is unofficial
  upstream; `&nf` alone is 9–22% worse area, `&dch -f; &nf` recovers to parity.
  A′ must confirm parity on Jacquard's designs before committing the flow change.
- **`sverilogparse` fork maintenance** — vendored submodule; carry the patch on a
  `-integration` branch per FORKED_DEPS_WORKFLOW; upstreamable (attribute capture
  is generally useful).
- **Does `\src` survive Yosys `flatten`** (separate from abc)? origin-shell
  targets the full flow, but confirm in A′/A0, since the on-ramp flattens.
- **Provenance granularity** — post-optimization a gate may map to 0/1/many source
  lines; the reporting (B3) and IR (B2) must not assume 1:1.
- **Asset size** — a second (provenance) wasm ~doubles the fetched-asset story;
  A2's opt-in-vs-default call interacts with Phase-4 distribution.

## Non-goals

- No change to the emulator AIG/boomerang core (ADR 0014/0015).
- Not blocked on upstream YoWASP adopting the patches (we build our own wasm).
- Full SVA / Verific-grade provenance is out of scope (bounded by the patched
  open-source toolchain).
