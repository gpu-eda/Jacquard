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
   for `--remote` tracking). Two commits added:
   `robtaylor/yosys@src-retention-y-ext` `bcc5698` (abc repoint; was `67137e21`, force-pushed
   once to fix trailer) and `robtaylor/yowasp-yosys@yowasp-yosys-integration` (yosys-src
   repoint, off `develop`). **The yowasp overlay's whole delta is this one repoint commit
   on `yowasp-yosys-integration`** — that's the branch the build must check out (NOT
   `develop`, which is the untouched upstream mirror kept for easy rebasing).
2. **← NEXT: Build the fork wasm** — clone `robtaylor/yowasp-yosys` at branch
   **`yowasp-yosys-integration`**, `git submodule update --init --recursive`, run
   `build.sh` (**CMake + wasi-sdk 33**, not the ADR's stale "27/Makefile"), hardcoded
   x86_64-linux → build in **Docker/Linux or CI**, not natively on macOS. `yosys-slang`
   is a separate `yosys-slang-src` submodule (pulled recursively). No fork/repoint work
   remains before this — it's the head of the queue now.
3. **Run the abc_new flow on the fork wasm** on a small seq+comb design; confirm it
   (a) maps flops without the `ff.cc` error, (b) QoR parity vs classic, (c) carries
   `\src` on mapped cells (coverage %). Drop `-noattr`; set `scratchpad -set
   abc9.origins_max N`.
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
