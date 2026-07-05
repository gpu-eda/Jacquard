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
2. **✅ IN PROGRESS (2026-07-06) — A0 CI build authored + running.**
   `robtaylor/yowasp-yosys/.github/workflows/provenance-wasm.yml` (on push to
   `yowasp-yosys-integration` + `workflow_dispatch`), two jobs:
   - **build** — compiles the patched fork to WASM under WASI, links yosys-slang,
     smoke-tests `read_slang` + `abc_new`, uploads `yosys.wasm` + wheel.
   - **provenance-check** — the go/no-go: runs origin-shell's validated `abc_new`
     origins flow (`scratchpad -set abc9.origins_max 100; abc_new -script +&dch,-f;&nf`)
     on `comb` + `seq2` (sequential) against sky130 (Jacquard's pinned volare hash
     `c6d73a35`), counts `\src` on mapped cells; **seq2 at 0% fails the job**.
   First run: `28758370102`. **Residual build risk CI will surface:** develop-0.64's
   yosys-slang pin (`4e53d772`, targets yosys 0.64/Apr) vs our fork's ~Jun-2026 yosys
   — slang may need a newer pin to compile. **NEXT once green:** read the coverage %
   (esp. seq2) → if high, we already hold the provenance wasm (A1 done); then wire
   `src/synth.rs` to the `abc_new` flow + drop `write_verilog -noattr`.
4. **In parallel: WS-B B0** — teach `sverilogparse` to capture `(* src *)` (fork
   `vendor/eda-infra-rs` per `~/.claude/FORKED_DEPS_WORKFLOW.md`), attach to
   `SVerilogCell` (`lib.rs:113`, constructed `sverilognom.rs:385`); prove with a
   hand-annotated netlist.

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
- Forks (all live + wired as of 2026-07-05):
  - `robtaylor/abc@origin-tracking-clean` (`2daf32f2`); abc#487 in review.
  - `robtaylor/yosys@src-retention-y-ext` (`bcc5698` — abc repoint added on top of the
    old `67137e21`).
  - `robtaylor/yowasp-yosys` (**new**, mirror of Codeberg `YoWASP/yosys`): default branch
    `develop` (clean mirror); **`yowasp-yosys-integration`** carries the `yosys-src` repoint.
    Codeberg PR refs (`refs/pull/*`) were rejected by GitHub on mirror-push — harmless.
- yosys fork's abc pointer: ✅ now `robtaylor/abc@origin-tracking-clean` (repoint done).

---
**Resume with:** `/resume_handoff docs/handoffs/rtl-source-provenance-handoff.md`
