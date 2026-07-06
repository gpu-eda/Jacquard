# Handoff — WS-A integration: `synth.rs` → `abc_new` origins flow

**Created:** 2026-07-06
**Branch:** `main` (all prerequisites below are committed + pushed; this thread is
**not started** — it's the next WS-A step of ADR-0021 Phase 2 provenance).
**Parent thread:** [`rtl-source-provenance-handoff.md`](rtl-source-provenance-handoff.md)
(overall Phase-2 status); this handoff is the focused next-step for the on-ramp.

## Goal

Make the **on-ramp itself emit `\src`.** Today `jacquard sim design.v …` synthesizes
behavioral RTL through `src/synth.rs`, but that flow **strips** provenance. Switch it
to the origins-carrying `abc_new` std-cell flow (validated in A0) + the provenance
wasm + drop `-noattr`, so a synthesized gate-level netlist carries Yosys
`(* src *)` — which the now-complete ingestion side (WS-B B0–B2, B3 2/3) surfaces as
RTL source lines. This **closes the WS-A↔WS-B loop**: RTL in → source-annotated waves out.

## Why this is unblocked now

- **A0 is GO** (2026-07-06): the forked provenance wasm builds and carries origins
  through in-process WASI `abc_new` — **100% `\src` coverage on comb + seq2** (CI run
  `28779240456`, `robtaylor/yowasp-yosys/.github/workflows/provenance-wasm.yml`).
- **Ingestion is ready to consume it**: WS-B B0 (`sverilogparse` captures `(* src *)`),
  B1 (`netlistdb.cell_src`), B2 (`AIG::aigpin_src_locations`), B3 xsources +
  `--trace-signals` all landed on `main` and CI-green. They currently light up only
  for externally-provided provenance netlists; this change makes the *on-ramp* produce them.

## The three changes to `src/synth.rs`

The synth script is built in the `format!` at `src/synth.rs:221-250`. All three are
called out in the plan's WS-A reframe note.

1. **Swap classic `abc` → the `abc_new` origins flow.** Lines **`synth.rs:241` and
   `:244`** are `abc -liberty aigpdk_nomem.lib`. Replace with the A0-validated flow:
   `scratchpad -set abc9.origins_max <N>` then
   `abc_new -script +&dch,-f;&nf -liberty aigpdk_nomem.lib`.
   (Mirror origin-shell's `test/src_coverage.sh` / the provenance-wasm CI job, which
   also does `hierarchy -purge_lib` before `abc_new` to drop unused `abc9_box` cells —
   check whether the on-ramp's existing `synth -run coarse:` + `techmap` leaves such
   cells; add the purge if `abc_new` errors "no timing information".)
2. **Drop `-noattr`.** Line **`synth.rs:247`** is `write_verilog -noattr gatelevel.gv`.
   `-noattr` strips `\src`. Drop it (attribute-selective if other attrs cause noise).
3. **Use the provenance wasm.** `locate_yosys_wasm` (`synth.rs:256`) resolves
   explicit `--yosys-wasm` → `JACQUARD_YOSYS_WASM` env → a discovered `yowasp_yosys`
   Python install. The origins flow **requires the fork wasm** (`robtaylor/yowasp-yosys`);
   the stock/discovered wasm has `abc_new` but **not** the `&origins` patch, so it
   won't carry `\src`.

## ⚠️ Hard dependency + sequencing (read before starting)

**`abc_new` on the *stock* wasm FAILS on sequential logic** — the A′ spike hit
`ERROR: Bad connection $auto$ff.cc:337:...` on a single `posedge` flop. The fork
wasm's `ff.cc`/`memory_map`/`mem.cc` fixes (in `robtaylor/yosys@src-retention-y-ext`)
are what make `abc_new` even *function* on flops. **So you cannot just flip `synth.rs`
to `abc_new` against the stock/`yowasp-yosys`-pip wasm — it will break real (sequential)
designs.** This change is coupled to getting the provenance wasm to `synth.rs`:

- The provenance wasm is a CI artifact of `robtaylor/yowasp-yosys` (the
  `provenance-wasm (A0)` workflow uploads `yosys.wasm` + a `yowasp-yosys` wheel,
  14-day retention). That's **A1/A2** in the plan (harden/pin + distribute).
- **Recommended two-step:** (a) wire the `abc_new`+provenance path in `synth.rs`
  **behind a flag / opt-in**, and in CI point `JACQUARD_YOSYS_WASM` at the A0-built
  wasm to prove the on-ramp emits `\src` end-to-end (behavioral RTL → `sim` → a signal
  resolves to its RTL line via the WS-B chain); (b) make it the default only once A2
  distribution lands (fetch-from-release, noted as a follow-up at `synth.rs:254`).
  Do **not** make `abc_new` the default while the resolved wasm can be the stock one.

## Obligations / risks

- **QoR validation (plan obligation).** `abc9` std-cell mapping is "the road less
  travelled" upstream (`YosysHQ/yosys#5679` removed `abc9 -liberty`); bare `&nf` is
  9–22% worse area, `&dch -f; &nf` recovers to ~parity. **A0 validated `\src`
  coverage, NOT QoR on Jacquard's designs.** Compare cell count / area (`stat`) of the
  `abc_new` flow vs the classic-`abc` baseline on the on-ramp's fixtures
  (counter/assert/mem designs; the committed `*_synth.gv` fixtures) before defaulting.
- **`flatten` + `\src`.** The on-ramp `flatten`s (`synth.rs:230`); confirm `\src`
  survives flatten (separate from abc). origin-shell targets the full flow; verify.
- **On-ramp fixture regen.** The committed `*_synth.gv` test fixtures were each written
  once (via `--emit-synth`); if the flow change alters them, regenerate deliberately.

## Prerequisites already in place (don't redo)

- Provenance fork chain, all wired + verified (see parent handoff): `robtaylor/abc@origin-tracking-clean`
  `3632a04a` → `robtaylor/yosys@src-retention-y-ext` `fd151be` → `robtaylor/yowasp-yosys@yowasp-yosys-integration`.
- A0 CI (`provenance-wasm.yml`) builds + coverage-checks the wasm; green.
- WS-B ingestion (B0–B2, B3 xsources+trace-signals) on Jacquard `main`.

## References

- Plan: [`docs/plans/rtl-source-provenance.md`](../plans/rtl-source-provenance.md)
  — WS-A section (esp. the reframe note listing these three `synth.rs` changes),
  A0 GO result, A1/A2.
- ADR: [`docs/adr/0021-behavioral-rtl-support.md`](../adr/0021-behavioral-rtl-support.md).
- On-ramp docs: [`docs/accepted-rtl.md`](../accepted-rtl.md), `docs/synthesis-flow.md`
  (the manual flow `synth.rs` wraps).
- origin-shell (cribbed flow + coverage harness): `https://github.com/robtaylor/origin-shell`
  (`test/src_coverage.sh`, `SRC-TRACKING-POC.md`).

---
**Resume with:** `/resume_handoff docs/handoffs/synth-abc-new-integration-handoff.md`
