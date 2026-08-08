# Plan — RTL on-ramp folded into `sim`/`cosim`

**Status:** Active — reworks draft PR [#167](https://github.com/gpu-eda/Jacquard/pull/167)

**Decisions:** [0021](../architecture/decisions/0021-behavioral-rtl-support.md) (Revised 2026-07-03 —
synth folds into `sim`/`cosim`, no `build` command), aligns with
[0019](../architecture/decisions/0019-cell-model-ir.md) (descriptor-supplied logic + timing) and
[0018](../architecture/decisions/0018-distribution-and-installation.md) (wasm distribution).

**Predecessors:** #167 shipped the `src/synth.rs` embedded-Yosys engine and a
`jacquard build` subcommand + `synth` feature (CI green, draft). This plan keeps
the engine and **removes the standalone command**, wiring synthesis into the
simulation input path instead.

**Tracking:** [#162](https://github.com/gpu-eda/Jacquard/issues/162).

---

## Goal

Behavioral RTL becomes a first-class **one-command** input:
`jacquard sim design.v in.vcd out.vcd N` (and the `cosim` equivalent) detects
behavioral RTL, synthesizes it transparently to an aigpdk netlist (cached), and
simulates — no separate `build` step, no Python, no external toolchain. A
pre-synthesized netlist continues to simulate unchanged.

## Input dispatch (the core behaviour — Decision 0021 §1)

On the netlist-input path of `sim`/`cosim`:

1. Attempt structural parse (`NetlistDB::from_sverilog_file`).
2. **Parse succeeds** → enumerate instantiated cells, test against the built-in
   stdcell recognizers (the `is_*_stdcell` helpers introduced by #160 / Decision 0019):
   - **All recognized** → gate-level, built-in PDK → **simulate directly.**
     Logic + timing come from the embedded descriptor (`--corner`); no `.lib`.
   - **Any unrecognized** → gate-level, unknown PDK → **error**, listing the
     unrecognized cell types: *"gate-level netlist with unrecognized cells —
     pass `--cell-descriptor <path>`"* (Decision 0019 D8). **Do not synthesize.**
3. **Parse fails** → treat as behavioral → **synthesize** (embedded Yosys) →
   parse the result → simulate. If synthesis *also* fails, surface **both** the
   structural parse error and the Yosys diagnostics (a genuine netlist syntax
   error must not masquerade as "tried to synthesize your RTL").

**Overrides:** `--rtl` forces the synth path; `--netlist` forces
direct-structural (skip detection). **Transparency:** always print the decision,
including the QoR tier on the synth path (e.g.
`design.v: behavioral RTL → synthesized [YoWASP Yosys, functional QoR] → <cache>`).

> Detection is a heuristic on a structural-only parser; the explicit flags are
> the escape hatch. Pure net-alias `assign`s parse structurally (they occur in
> gate-level too) — only real behavioral constructs (`always`, arithmetic/logic
> `assign`, `if`/`case`) trip the parse and route to synth. Confirm the exact
> `is_*_stdcell` API against `src/` during Phase 1 (do not assume names).

---

## Phases

### Phase 1 — Fold synth into `sim`/`cosim`; remove `build`

**Entry:** #167 branch (`feat/rtl-onramp-build`), `synth` feature + `src/synth.rs`
present.

- Extract the synthesis invocation from `cmd_build` into a reusable
  `synth::synthesize(design: &Path, opts) -> Result<PathBuf>` returning the
  cached gate-level `.gv` path. Cache keyed by content hash of (design sources +
  synth script + yosys.wasm hash), under `$XDG_CACHE_HOME/jacquard` alongside the
  compiled-module cache.
- **Use `read_slang` for the SV frontend.** The pinned `yowasp-yosys
  0.64.0.0.post1131` wasm bundles **yosys-slang** (verified: `read_slang` +
  495 `slang` symbols in the 39 MB module) — a near-complete SV-2017 elaborator,
  far beyond built-in `read_verilog -sv`. Update `synth_script` to front SV input
  with `read_slang` (fall back to `read_verilog -sv` if `read_slang` errors or is
  absent in an older wasm — probe `help read_slang` once and cache the result, so
  the on-ramp degrades gracefully on a pre-slang module).
- Add the input-dispatch classifier (above) to the shared `sim`/`cosim` netlist
  load path.
- Add CLI flags to `SimArgs` / `CosimArgs`: `--emit-synth <path>` (dump the
  intermediate netlist), `--rtl`, `--netlist`.
- **Delete** `Build`, `BuildArgs`, `cmd_build` from `src/bin/jacquard.rs`.
- Graceful message when the binary is built without `--features synth` and given
  behavioral input: point at the synth-enabled build / release binary.

**Exit:** `jacquard sim counter.v in.vcd out.vcd 1 --features synth` runs
end-to-end (behavioral → waves); the three #167 validation designs (counter,
assert_test, mem_test) simulate from RTL directly; a gate-level fixture still
simulates with the feature **off**; an unknown-cell netlist errors toward
`--cell-descriptor`; `--emit-synth` writes a parseable `.gv`; no `build`
subcommand exists.

### Phase 2 — CI coverage + distribution (blocking: feature ships in no binary)

**Entry:** Phase 1 merged to the branch.

- Add `--features synth` to `release.yml` and `user-acceptance.yml` so shipped
  binaries include the on-ramp.
- Add a CI job to `ci.yml`: `cargo build --release --features synth` + a
  `jacquard sim <behavioral.v>` smoke run. Resolve `yosys.wasm` sourcing in CI
  (install a pinned `yowasp-yosys` wheel and discover it, matching local
  verification; or fetch the release asset once increment 2 lands).
- Confirm `--features synth` compile time is tolerable in CI (cache the cranelift
  build if needed).

**Exit:** CI proves `sim` consumes behavioral RTL; release/UA binaries contain
the feature; green on the branch.

### Phase 3 — Docs + stale-reference cleanup

**Entry:** Phases 1–2 green.

- Rewrite the RTL flow in `docs/getting-started.md`, `docs/installation.md`, and
  any `synthesis-flow.md` cross-links to the single-command story.
- **Write `docs/accepted-rtl.md` — the accepted behavioral-RTL surface.** The
  honest framing: what `sim`/`cosim` accept as behavioral input is exactly what
  the embedded **YoWASP Yosys frontend synthesizes** — and that frontend bundles
  **yosys-slang** (`read_slang`, verified in the pinned 0.64 wasm), *plus* the
  project's techmaps and *minus* testbench-only constructs. Document:
  - **Supported (delegated to Yosys + slang):** synthesizable Verilog-2005 and a
    broad **SystemVerilog-2017** surface via slang (packages, interfaces,
    structs/enums, `always_ff`/`always_comb`, parameters, advanced generate,
    memories) — not the narrow built-in `read_verilog -sv` subset.
  - **Project-specific mappings:** immediate assertions → `GEM_ASSERT`
    (`--strip-assertions` removes via `chformal`); `$display` → `GEM_DISPLAY`;
    inferred memories → `RAMGEM` via `memlib_yosys.txt`.
  - **Known limits:** **concurrent-SVA → checker synthesis is partial** (a Yosys
    formal-flow bound, *independent* of slang's parsing; #106/#107);
    testbench-only constructs (`#delays`, most `initial`, TB `$display`) are
    dropped by synthesis, not simulated.
  - State plainly that the *authoritative* accepted-surface is the empirical
    coverage table (Phase 4), not this prose — prose is the orientation.
  - Cross-link from `getting-started.md`.
- Fix stale `jacquard map` references (`CLAUDE.md`, docs) → `dump-paths`.
- `docs/plans/cell-model-ir.md` status — **reconciled with `main`** during rebase:
  its status is now "Largely delivered" (C3 + the D5 L4-from-descriptor runtime
  wiring landed on `main` via `de8255f3`), so no further edit here.
- Update `docs/handoffs/adr-0021-behavioral-rtl-handoff.md` (or resolve/fold it
  per handoff-discipline once this ships).

**Exit:** No doc describes a `jacquard build` command or the old multi-tool
ceremony; `map`/`build` stale refs gone.

### Phase 4 — Follow-ups (deferred, not gating)

- **Load exception-handling wasm** — newer `yowasp-yosys` wheels build the wasm
  with the WebAssembly exception-handling proposal, which our `wasmtime` engine
  (`Engine::default()`, no `gc` feature) rejects: *"exception refs not supported
  without the exception handling feature."* `Config::wasm_exceptions(true)` exists
  but is `#[cfg(feature = "gc")]`, so it needs the `gc` feature **and** a spike
  confirming wasm-EH actually *runs* yosys (not just parses). Until then CI + docs
  pin `yowasp-yosys==0.64.0.0.post1131`. Removing that pin is the exit criterion.
- **Fetch `yosys.wasm` from GitHub release** (increment 2, Decision 0018): publish the
  pinned wasm as a Jacquard release asset; first behavioral run fetches to cache
  + sha256-verifies.
- **`--synth-target sky130|gf180`** — synthesize to a real PDK (uses #160
  descriptors) for timing-accurate on-ramp runs.
- **Empirical SV/Verilog coverage table** — turn the `docs/accepted-rtl.md`
  prose surface into a measured pass/fail matrix by running
  [SymbiFlow/sv-tests](https://github.com/SymbiFlow/sv-tests) (or a curated
  subset) through the embedded YoWASP Yosys frontend and recording which
  constructs synthesize. Because the accepted surface *is* the Yosys frontend,
  this is the only trustworthy way to enumerate it (vs hand-claiming a feature
  list). Publish the table into `docs/accepted-rtl.md`; automatable as a CI job
  so the coverage claim stays current as the pinned `yosys.wasm` moves.
- ~~**Wire L4-from-descriptor onto the runtime timing path**~~ — **landed on
  `main`** (`de8255f3`, Decision 0019 D5): all runtime timing paths now source L4 from
  the cell-model-IR descriptor, so `--corner` on-ramp timing for built-in PDKs is
  live. No longer a follow-up.
- **Project manifest (`Jacquard.toml`)** — collapse the positional
  `sim netlist in.vcd out.vcd N` arg soup and hold synth-target/top/sources,
  referencing the existing `sim_config.json`. Its own decision when scheduled.

---

## Non-goals

- No config/manifest format this pass (Phase 4).
- No Phase-2 `\src`-provenance work (Decision 0021 Phase 2 roadmap, unchanged).
- No change to the AIG/boomerang core or `sverilogparse` (Decision 0021 §4).
