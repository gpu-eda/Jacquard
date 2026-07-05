# Plan — RTL-source provenance (ADR 0021 Phase 2)

**Status:** Active — design captured; **WS-A gated on A0** (build the forked wasm,
check `\src` survives in-process abc — the spike *is* the build). Large,
dependency-gated; not started.

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

### A0 — GATING: build the forked wasm, check `\src` survives (the spike **is** the build)

**The one unproven thing the workstream rests on:** does `\src` survive abc's
std-cell mapping? origin-shell validated retention only through **external** abc
(`ABCEXTERNAL`, temp-`.aig` round-trips). But **in WASM the external/internal abc
distinction does not exist** — WASI has no `exec`, so abc is *always* in-process
(compiled in-tree; verified in the shipped module). So there is **no separate
native spike**: a native in-process build wouldn't faithfully model the wasm abc,
and the wasm build is work we'd do anyway (it's the A1 artifact). The spike **is**
building the forked wasm — no throwaway work, and the real target is tested
directly.

**Build inputs (recon'd against the live Codeberg `YoWASP/yosys`, 2026-07-04).**
The two load-bearing forks **already exist** (`robtaylor/yosys@src-retention-y-ext`,
`robtaylor/abc@origin-tracking-clean` #487). The build repo's `.gitmodules` has
`yosys-src` (→ `YosysHQ/yosys`) and a **separate `yosys-slang-src`** (→
`povik/yosys-slang`, the SV frontend); **abc is *nested* inside `yosys-src`**, not
a top-level submodule. So the repoints are:
1. **`yowasp` overlay:** `yosys-src` → `robtaylor/yosys@src-retention-y-ext` (one
   repoint). `robtaylor/yowasp-yosys` **does not exist yet** — create it.
2. **Inside the yosys fork — NOT yet done:** `robtaylor/yosys@src-retention-y-ext`
   still points its `abc` submodule at **`YosysHQ/abc`** (upstream), so the
   origins patches aren't wired in. **A0 step-0: repoint the yosys fork's `abc` →
   `robtaylor/abc@origin-tracking-clean`.** Without this the build has yosys-side
   `\src` retention but stock abc, and provenance dies at mapping.

**Build mechanics (current, ≠ the ADR's stale description):** `build.sh` is
**CMake** driving `wasi-sdk-p1.cmake` — `wasi-sdk **33**` (not 27), no
Makefile/`CONFIG=wasi`/flex/LTO; disables zlib/libffi/readline/editline/tcl; uses
ccache. It hardcodes the **x86_64-linux** wasi-sdk, so building on this Apple-Silicon
Mac needs the macOS wasi-sdk variant or a **Linux/Docker/CI** build (prefer CI —
it's the A1 reproducible-build home anyway).

- Repoint (steps 1–2 above), run `build.sh`, synthesize a tiny multi-module design
  to aigpdk; inspect the output for `(* src=… *)` on mapped cells.
- **Go/no-go:** `\src` present + correct on ≥ most mapped cells → **you already
  hold the provenance wasm** (A1 done); proceed to harden (A1) + distribute (A2).
  Absent/scrambled → the defect is abc's in-process `&origins` handling; take a
  fallback before investing further.
- **Fallbacks if it fails:** (a) fix `&origins` on abc's in-process entry points
  (extend abc#487) — most direct, since in-process keeps everything in one memory
  space (no `.aig` serialization to lose it, so this may be *easier* than the
  validated external path, not harder); (b) Nix **native** toolchain using the
  validated external-abc path; (c) native-only provenance, on-ramp wasm stays
  provenance-free.

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

### B1 — Carry source location through `netlistdb`

Add an optional per-cell (and where meaningful, per-net) `source_loc` on
`NetlistDB`, populated from B0. Keep it `Option` — most cells post-synthesis may
have 0 provenance.

- **Exit:** `NetlistDB::from_sverilog_file` exposes `\src` for annotated cells;
  zero overhead / behaviour change when absent.

### B2 — Preserve provenance through AIG / staging / flatten

Thread a mapping from AIG nodes / endpoint groups back to `\src` so a
sim-visible signal can be traced to a source line. Provenance is **lossy** by
nature (abc merges/splits nodes): design for **0, 1, or many** source locations
per signal; never assert exactly one.

- **Exit:** given an annotated netlist, a chosen output/endpoint resolves to its
  source location(s) through the built `FlattenedScript`.

### B3 — Surface `\src` in the user-facing outputs

Add source locations to the three consumers, gated on availability (fall back to
today's hierarchical gate name when absent):
- `--trace-signals` (`docs/signal-tracing.md`) — annotate traced signals.
- Timing-violation reports (`docs/timing-violations.md`).
- X-debugging `xsources` / `xroots` (`docs/x-debugging.md`) — the highest-value
  target: report the RTL line of an X-source, not the flattened gate.

- **Exit:** an end-to-end run on a synthetic annotated netlist prints source
  locations in all three; a design with no provenance is unchanged.

---

## Sequencing

```
A0 build+check ─(go)─► A1 harden/pin ──► A2 distribute ─┐
                                                     ├─► integrate: on-ramp emits + surfaces \src
B0 parser ─► B1 netlistdb ─► B2 AIG ─► B3 outputs ──┘   (validated on synthetic netlist first)
```

- **Parallel:** A0 and B0–B3 have no dependency; B is provable against a
  hand-written annotated netlist.
- **Barrier:** full end-to-end (RTL → provenance wasm → surfaced source lines)
  needs A1+A2 *and* B0–B3.
- **Kill-switch:** if A0 fails, WS-B still delivers value for any *externally*
  produced provenance netlist (DC/native Yosys with origins); the on-ramp just
  doesn't auto-generate it until a fallback toolchain lands.

## Risks / open questions

- **A0 in-process `&origins` survival** — the gating unknown (see A0).
- **`sverilogparse` fork maintenance** — vendored submodule; carry the patch on a
  `-integration` branch per FORKED_DEPS_WORKFLOW; upstreamable (attribute capture
  is generally useful).
- **Does `\src` survive Yosys `flatten`** (separate from abc)? origin-shell
  targets the full flow, but confirm in the A0 spike, since the on-ramp flattens.
- **Provenance granularity** — post-optimization a gate may map to 0/1/many source
  lines; the reporting (B3) and IR (B2) must not assume 1:1.
- **Asset size** — a second (provenance) wasm ~doubles the fetched-asset story;
  A2's opt-in-vs-default call interacts with Phase-4 distribution.

## Non-goals

- No change to the emulator AIG/boomerang core (ADR 0014/0015).
- Not blocked on upstream YoWASP adopting the patches (we build our own wasm).
- Full SVA / Verific-grade provenance is out of scope (bounded by the patched
  open-source toolchain).
