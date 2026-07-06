# Handoff — ADR 0021 Phase 2: RTL-source provenance

**Updated:** 2026-07-05
**Branch:** `main` (all work below is committed + pushed to `origin/main`)
**Working tree:** clean except the pre-existing untracked `.env`

## Where things stand

**ADR 0021 Phase 1–3 (the RTL on-ramp) is DONE and MERGED** (PR #167, rebase-merged
to main). `jacquard sim design.v in.vcd out.vcd N` now takes behavioral RTL:
synthesis is folded into `sim`/`cosim` (no `build` command), via embedded YoWASP
Yosys with `read_slang`; released binaries build `--features synth`; CI exercises
it (Linux compile job + a Metal on-ramp smoke asserting bit-identical waves vs the
committed fixture). Docs: `docs/accepted-rtl.md`. **That thread is resolved** — the
old `docs/handoffs/adr-0021-behavioral-rtl-handoff.md` describes it and should be
**deleted** (its content is folded into ADR 0021 + `docs/plans/rtl-onramp-sim-integration.md`).

**Phase 2 (provenance) is design-captured, not started.** The plan is
`docs/plans/rtl-source-provenance.md`. This handoff is the ephemeral working state
on top of it.

## THE key finding this session (A′ spike, 2026-07-05)

The plan's original "de-risk the `abc_new` flow on the *stock* wasm first (A′),
then build the patched wasm (A0)" split **partially collapsed**. Ran origin-shell's
mapping invocation on the **stock `yowasp-yosys 0.64`** (scratchpad `/tmp/claude/aprime`,
`prov_test.v` + `classic.ys`/`abcnew.ys`):

- `abc_new -script +&dch,-f;&nf -liberty aigpdk_nomem.lib` on a **combinational**
  design → **works**.
- Same on a **sequential** design (one `posedge` flop) → **FAILS**:
  `ERROR: Bad connection $auto$ff.cc:337:slice$150/D ~ \d [1]` (abc_new is
  "experimental" in stock).
- classic `abc` baseline (what `src/synth.rs` uses today) → works, 33 cells.

That `ff.cc` error is exactly the flop-handling `robtaylor/yosys@src-retention-y-ext`
fixes (origin-shell: "the `ff.cc`/`memory_map`/`mem.cc` fixes"). **So the yosys fork
is required even to make `abc_new` function on flops — not just to carry origins.**
A′ can only validate the combinational sub-case on stock; **A′ effectively folds
into A0** (build the fork wasm, then test flow-correctness + QoR + origins together).

## Critical context (why Phase 2 is shaped the way it is)

- **Two independent problems.** (WS-A) the toolchain must *emit* `\src`; (WS-B)
  Jacquard must *ingest* it — today `sverilogparse` **discards all attributes**
  (`vendor/eda-infra-rs/sverilogparse/src/sverilognom.rs:44`, "we regard attributes
  as comments") and `netlistdb` has no source field. WS-B is provable NOW against a
  hand-annotated `(* src *)` netlist, independent of the toolchain.
- **The real gate is the `abc9`/`aiger2` XAIGER-`"y"` channel, NOT "in-process
  abc".** Origins ride an in-memory AIGER round-trip keyed by object id, so
  external-vs-in-process is irrelevant. origin-shell's lesson: provenance *cannot*
  survive the classic `abc`/BLIF path (BLIF has no object identity) — the std-cell
  flow must move to `abc_new` (`write_xaiger2 → ABC &read;&origins;&dch -f;&nf →
  read_xaiger2`).
- **`src/synth.rs` must change regardless of the wasm:** it maps with classic `abc
  -liberty` (lines 241/244) → must become the `abc_new` flow, and it writes with
  **`write_verilog -noattr`** (line 247) which **strips `\src`** — must drop `-noattr`.
- **QoR obligation:** `abc9` std-cell mapping is "the road less travelled" upstream
  (`YosysHQ/yosys#5679` removed `abc9 -liberty`). Bare `&nf` is 9–22% worse area;
  `&dch -f; &nf` recovers to parity — must be validated.
- **The `\src`-coverage test:** crib origin-shell's `test/src_coverage.sh` (loads
  `write_json`, counts `'src' in cell['attributes']` over mapped cells) — the A0
  go/no-go metric.

## Next-up (priority order)

1. **✅ DONE (2026-07-05) — Fork prerequisites wired + verified end-to-end.**
   The full submodule chain is live:
   ```
   robtaylor/yowasp-yosys @ yowasp-yosys-integration   ← NEW repo (mirror of Codeberg
     └─ yosys-src → robtaylor/yosys@src-retention-y-ext   YoWASP/yosys@develop); default
          └─ abc  → robtaylor/abc@origin-tracking-clean   branch = develop (clean mirror)
          └─ yosys-slang-src → povik/yosys-slang (upstream, unchanged)
   ```
   Pins: yosys-src gitlink `bcc5698` == `src-retention-y-ext` HEAD; abc gitlink
   `2daf32f2` == `origin-tracking-clean` HEAD (both `.gitmodules` `branch=` keys set
   for `--remote` tracking). **The `yowasp-yosys-integration` branch is based on
   `develop-0.64`, NOT `develop`** — see the ⚠️ build-recipe correction below; that's
   the branch the build checks out (`develop` stays an untouched upstream mirror).
   ⚠️ **BUILD-RECIPE CORRECTION (2026-07-06) — the plan/ADR's "CMake + wasi-sdk 33"
   is WRONG for our fork.** Two hard constraints force the `develop-0.64` Makefile
   recipe instead:
   - `robtaylor/yosys@src-retention-y-ext` is **Makefile-only (no `CMakeLists.txt`)**,
     so `develop`'s `cmake -S yosys-src` cannot build it at all.
   - `develop`'s CMake migration (`61073ed`) **dropped yosys-slang**, so its wasm has
     **no `read_slang`** — the SV frontend the on-ramp depends on.
   `develop-0.64` = **Makefile (`CONFIG=wasi`, `-flto`) + wasi-sdk 27 + flex-from-source
   + yosys-slang whole-archive** — the exact recipe that built the pinned `yowasp-yosys
   0.64` the on-ramp uses today. Our only delta is the yosys-src repoint.
2. **✅ A0 CI authored; BUILD JOB GREEN (2026-07-06).**
   `robtaylor/yowasp-yosys/.github/workflows/provenance-wasm.yml` (push to
   `yowasp-yosys-integration` + `workflow_dispatch`), two jobs:
   - **build** — compiles the patched fork to WASM under WASI, links yosys-slang,
     smoke-tests `read_slang` + `abc_new`, uploads `yosys.wasm` + wheel. **✅ GREEN**
     as of run `28760083027` — the ~Jun-yosys × slang-pin skew risk did NOT bite;
     slang + abc_new both compile + are present in the wasm.
   - **provenance-check** — ✅ **GREEN (A0 GO), run `28779240456`.** Runs origin-shell's
     validated `abc_new` origins flow (`read_liberty -lib`; proc/opt/memory/techmap;
     `dfflibmap`; **`hierarchy -top <d> -purge_lib`** — load-bearing, purges unused
     `abc9_box` cells that otherwise error "no timing info"; `scratchpad -set
     abc9.origins_max 100`; `abc_new -script +&dch,-f;&nf -liberty`) on `comb` + `seq2`
     against sky130 (Jacquard's pinned volare `c6d73a35`; installs at
     `$PDK_ROOT/volare/sky130/versions/<hash>/…` — note the double `volare`), counts
     `\src` on mapped cells. **Result: `comb 4/4 = 100%`, `seq2 88/88 = 100%`.**

   **🎯 A0 RESULT — origins survive the WASI in-process `abc_new` round-trip (100% on
   sequential seq2).** This resolves the ADR/plan's central open risk ("does `&origins`
   survive the *in-process* abc call path, not just external temp-file abc?") — **YES**.
   Per the plan, high coverage ⇒ **we hold the provenance wasm (A1's core artifact done)**.

   **⚠️ KEY BLOCKER FOUND + FIXED — abc was 97 commits behind, missing WASI guards.**
   The first build (`28758370102`) failed at the WASI link:
   `wasm-ld: error: yosys-libabc.a(NtkNtk.o): undefined symbol: system`. Root cause:
   `robtaylor/abc@origin-tracking-clean` was based on `berkeley-abc/master` @
   `bef23270` (2026-03-28) — **8 ahead** (the #487 vOrigins/&origins commits) but
   **97 behind**, and those 97 upstream commits carry berkeley-abc's `#ifdef __wasm`
   guards around every `system()` call (`NtkNtk.cpp`, `utilSignal.c`, `abcPart.c`, …).
   Stock YoWASP builds because yosys 0.64's abc (`YosysHQ/abc@180a6adb`) already has
   the guards. **Fix (done, Rob-approved):** rebased the 8 origins commits onto current
   `berkeley-abc/master` — **conflict-free** — and cascaded the new SHAs:
   - `robtaylor/abc@origin-tracking-clean`: `2daf32f2` → **`3632a04a`** (force-pushed;
     this also freshens **abc#487**, which was "maintainer_awaiting_reply").
   - `robtaylor/yosys@src-retention-y-ext` abc gitlink → `3632a04a`; HEAD `bcc5698` → **`fd151be`**.
   - `robtaylor/yowasp-yosys@yowasp-yosys-integration` yosys-src gitlink → `fd151be`.
   Lesson: the abc origins fork lives on `berkeley-abc` (correct for #487) and must be
   kept rebased on master to inherit WASI/build fixes — freezing it breaks the wasm build.

   **NEXT once provenance-check is green:** read the `\src` coverage % (esp. seq2 — the
   sequential go/no-go). High → we already hold the provenance wasm (A1 done); then wire
   `src/synth.rs` to the `abc_new` flow + drop `write_verilog -noattr`.
4. **✅ DONE (2026-07-06) — WS-B B0: `sverilogparse` captures `(* src *)`.**
   `gpu-eda/eda-infra-rs@jacquard-integration` `b518f55` → **`a0772f4`**; Jacquard
   submodule pin bumped (Jacquard `main` @ `6e08bc71`). `SVerilogCell` gains
   `src: Option<CompactString>`. Key design: `skip_whitespace_and_comment` **no
   longer eats `(* ... *)`** (it treated them as comments) — attributes are captured/
   skipped explicitly at the 3 grammar leading edges (`module` decl, `port_item`,
   module-body loop) via `leading_attributes`. This narrows accepted input to
   "attributes only at those positions" — sound for Yosys/DC structural netlists.
   `fmt::Display` re-emits `src` (round-trips). Test `test_src_attribute` +
   `sverilogparse/tests/attributes.v` prove capture (src-on-own-line, src-not-first
   in a multi-attr block, no-src cell) + round-trip. `/simplify` run (short-circuit
   `extract_src_attr`, shared `is_ident_char`, rename). All sverilogparse +
   netlistdb tests pass; Jacquard builds.
5. **✅ DONE (2026-07-06) — WS-B B1: `netlistdb` carries `\src`.**
   `gpu-eda/eda-infra-rs@jacquard-integration` `a0772f4` → **`4f7f0ec`**; Jacquard pin
   bumped (`main` @ `984c1036`). `NetlistDB` gains **`cell_src: Vec<Option<CompactString>>`**,
   parallel to `celltypes`/`cellnames`, populated in `insert_cell` (`builder.rs:156`)
   from `SVerilogCell.src` through hierarchy flattening. `None` for the top cell
   (idx 0) and synthesised inverters (`assign` inversions, `builder.rs:358`);
   `Option`-typed and `None`-heavy → zero behaviour change when absent. Test
   `netlistdb/tests/provenance.{rs,v}` builds a DB from an annotated netlist and
   asserts `cell_src` is parallel to the cell arrays, annotated cell carries its
   `src`, un-annotated/top cells are `None`. All netlistdb + sverilogparse tests pass;
   Jacquard builds.
6. **✅ DONE (2026-07-06) — WS-B B2: `\src` resolves through the AIG.**
   Jacquard `main` @ **`68cc938b`** (Jacquard-only; no submodule change). Added
   **`AIG::aigpin_src_locations(aigpin, netlistdb) -> Vec<CompactString>`**
   (`src/aig.rs`). **Key win:** the AIG already carried a per-pin cell-origin map
   (`aigpin_cell_origins: Vec<Vec<(cell_id, type, pin)>>`, built for SDF
   back-annotation) — so B2 just *composes* it with `netlistdb.cell_src` (B1); **no
   new state threaded through the 5500-line builder.** `cell_id` from the origins
   indexes `cell_src`. Handles **0/1/many** naturally (origins accumulate; internal
   AND nodes / un-annotated → `[]`), de-duplicated. A primary output (aigpin in
   `primary_outputs`) resolves directly. Tests in `path_mapping_tests` (+ fixture
   `tests/timing_test/sky130_timing/prov_annotated.v`): annotated nand2 resolves;
   un-annotated `inv_chain` → `[]` everywhere. All 30 aig tests pass.
7. **⏳ IN PROGRESS (2026-07-06) — WS-B B3: surface `\src` in the three consumers.**
   **2 of 3 done** (Jacquard `main`, commits below):
   - ✅ **`xsources`** (`ec7b648f`) — `src/sim/x_sources.rs`. Added optional `src` to
     the `XSource` record, populated from `cell_src[cell_id]` (DFF/SRAM sources; the
     X-source *is* a cell, so direct `cell_src` lookup, not the aigpin resolver).
     Undriven inputs = ports → `None`. Schema `1.0`→`1.1` (additive, skip-if-none).
     **Bonus:** the committed `tests/xprop_cosim/xprop_demo_synth.gv` already carries
     `(* src *)`, so provenance now flows through the *real* pipeline, not just the
     new fixture.
   - ✅ **`--trace-signals`** (`ca55c786`) — `src/sim/trace_signals.rs`. At
     registration, resolve the traced net's aigpin via `aig.aigpin_src_locations`
     (B2) and log `\`raw\` → RTL file:line`. Prints the source location per traced
     signal; no output change when absent.
   - ⏳ **timing-violation reports** — **NOT DONE (largest, governed schema).**
     Feasibility scoped: `word_id → cell_id` exists via `FlattenedScript.dff_constraints`
     (`c.cell_id`) in `build_word_symbol_map` (`flatten.rs:1852`); add `src` to
     `DffSiteName` (`flatten.rs:128`) from `cell_src[c.cell_id]`, thread into the
     `WordSymbolMap`-backed resolver and onto `ViolationRecord`/`PerWordSummary`
     (`timing_report.rs:31/90`). **A state-word packs multiple DFFs**, so a
     violation's `src` is itself **0/1/many** — collect distinct. The timing-report
     JSON is **governed by ADR 0008** (`docs/adr/0008-structured-timing-output.md`),
     which *permits additive extensions* (line 93) — so bump `SCHEMA_VERSION`
     `1.2.0`→`1.3.0` and add an ADR 0008 note. Deliberately deferred rather than
     rushed at session end.
   B3 exit ("prints source locations in all three") reached for xsources +
   trace-signals; timing-report remains.

## Artifacts / references

- Plan: [`docs/plans/rtl-source-provenance.md`](../plans/rtl-source-provenance.md)
  (has the full WS-A/WS-B breakdown, fork/pinning strategy, corrected build facts).
- ADR: [`docs/adr/0021-behavioral-rtl-support.md`](../adr/0021-behavioral-rtl-support.md)
  Phase 2 section.
- origin-shell (cribbed): `SRC-TRACKING-POC.md`, `test/src_coverage.sh`,
  `DESIGN-stdcell-origin-tracking.md` — clone `https://github.com/robtaylor/origin-shell`
  (was at `/tmp/claude/origin-shell-recon` this session).
- A′ spike scratch: `/tmp/claude/aprime` (`prov_test.v`, `classic.ys`, `abcnew.ys`,
  `comb.ys` + logs) — **ephemeral `/tmp`, recreate from the plan if gone.**
- Forks (all live + wired; SHAs current as of 2026-07-06 after the abc rebase):
  - `robtaylor/abc@origin-tracking-clean` = **`3632a04a`** (8 origins commits rebased onto
    current `berkeley-abc/master`; carries the WASI `__wasm` guards). abc#487 in review.
  - `robtaylor/yosys@src-retention-y-ext` = **`fd151be`** (abc gitlink → `3632a04a`;
    lineage `67137e21` → `bcc5698` abc-repoint → `fd151be` abc-rebase-bump).
  - `robtaylor/yowasp-yosys` (mirror of Codeberg `YoWASP/yosys`): default `develop`
    (clean mirror); **`yowasp-yosys-integration`** (base `develop-0.64`) carries the
    `yosys-src` repoint (→ `fd151be`) + the A0 CI workflow. Codeberg PR refs
    (`refs/pull/*`) were rejected by GitHub on mirror-push — harmless.
- Full chain verified consistent: yowasp yosys-src `fd151be` == yosys HEAD `fd151be`;
  yosys abc `3632a04a` == abc HEAD `3632a04a`.

---
**Resume with:** `/resume_handoff docs/handoffs/rtl-source-provenance-handoff.md`
