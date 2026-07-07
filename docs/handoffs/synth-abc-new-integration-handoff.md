# Handoff — WS-A integration: `synth.rs` → `abc_new` origins flow + A2 fetch

**Updated:** 2026-07-07
**Branch:** `ws-a-abc-new-provenance` (pushed to origin; **1 commit** `42607dcc`).
**Status:** code done + committed; **BLOCKED** on a user PAT to publish the wasm
release, then end-to-end fetch validation, then docs + PR to `main`.
**Parent thread:** [`rtl-source-provenance-handoff.md`](rtl-source-provenance-handoff.md).

## What this thread achieved

Made the on-ramp **emit `\src`** (WS-A) and made jacquard **fetch the fork wasm by
default** (A2). The WS-A↔WS-B loop is closed: behavioral RTL → `sim` → a signal
resolves to its RTL line.

### DONE + validated (with the local fork wasm)

- **`synth.rs` synth script now maps via `abc_new`** (`src/synth.rs`, `synth_script`):
  - `abc -liberty` (×2) → single `abc_new -script +&dch,-f;&nf -liberty aigpdk_nomem.lib`
    + `scratchpad -set abc9.origins_max 100`.
  - Dropped `write_verilog -noattr` (it stripped `\src`).
  - **Two aigpdk-specific fixes the A0 CI (sky130-only) did NOT surface** — this was
    the real work:
    1. **`read_liberty -lib aigpdk_nomem.lib` at the top** — else `abc_new`'s XAIGER
       box path can't wire aigpdk flops: `ERROR: Bad connection $auto$ff.cc:337:.../CLK ~ \clk`.
    2. **`hierarchy{top_arg} -purge_lib` before `abc_new`** — else `Module 'BUF' with
       (* abc9_box *) has no timing information`.
    3. **Collapse the classic two-pass `abc; techmap; abc` to a single `abc_new` pass**
       — the second pass breaks XAIGER (`Bad connection $sc5/A ~ \rst`).
  - **Validated end-to-end** on a synchronous counter (`/tmp/claude/synthtest/counter.v`)
    with `JACQUARD_YOSYS_WASM`=local fork wasm: emitted netlist carries **29 `(* src *)`**
    → `counter.v` lines; GEM **simulates** it (5 cycles, valid netlist); `--trace-signals`
    resolves `count[0]` and internal net `_02_` → **`counter.v:11.5`**.
  - **QoR parity**: classic 24 cells vs abc_new 24 cells (same fork yosys, fair compare).

- **A2 fetch-from-release** (`locate_yosys_wasm` + `fetch_pinned_wasm` in `src/synth.rs`):
  resolution is now `--yosys-wasm` arg → `JACQUARD_YOSYS_WASM` env → **fetch pinned fork
  wheel** (download → verify sha256 → unzip → cache on first use). `discover_yowasp` is
  **removed** (a discovered stock wheel silently breaks abc_new on flops). Adds
  `ureq`/`sha2`/`zip` under the `synth` feature. Unit test `pinned_wasm_consts_consistent`
  guards URL⊇TAG + sha256 format. Pin (`src/synth.rs`):
  - `YOSYS_WASM_TAG = "yosys-wasm-0.63.0.0.post1135"`
  - `YOSYS_WASM_SHA256 = "fecc687f15270f25a44ea976dba7e5fc750336378d363ad60d8b37939e7125af"`
    (sha256 of the CI wheel `yowasp_yosys-0.63.0.0.post1135-py3-none-any.whl`).

- **Publish workflow** `.github/workflows/publish-yosys-wasm.yml` (`workflow_dispatch`):
  pulls the fork wheel from `robtaylor/yowasp-yosys` `provenance-wasm` **run
  `28779240456`** (artifact `provenance-yowasp-yosys-wheel`) and attaches it to a
  `gpu-eda/Jacquard` release via `softprops/action-gh-release`. Reads tag+sha256
  **straight from `src/synth.rs`** (single source of truth — no drift). Runs on a GitHub
  runner because **local upload from this env is infeasible** (~68 KB/s upstream vs
  GitHub's ~120 s asset-upload cap; an 8 MB asset dies at the cliff).

## ⚠️ BLOCKING NEXT STEP — user must create a PAT

The release can't be published without a cross-repo token (no repo secrets exist on
`gpu-eda/Jacquard`; the wheel lives in `robtaylor/yowasp-yosys` CI):

1. Create a **fine-grained PAT** with **`Actions: read`** on `robtaylor/yowasp-yosys`.
2. `gh secret set YOWASP_WASM_PAT --repo gpu-eda/Jacquard --body '<PAT>'`

## Remaining steps (after the PAT is set)

1. **Dispatch the workflow:** `gh workflow run publish-yosys-wasm.yml --ref ws-a-abc-new-provenance`
   → cuts release `yosys-wasm-0.63.0.0.post1135` with the wheel attached. (gpu-eda/Jacquard
   uses **immutable releases** — softprops handles the draft→publish flow; manual
   `gh release create ... <asset>` does NOT work here, and local upload hits the 120 s cap.)
2. **Validate the default fetch end-to-end:** clear `JACQUARD_YOSYS_WASM` + the cache
   (`~/.cache/jacquard/wasm/`), run `jacquard sim counter.v … --trace-signals` → confirm
   it downloads the wheel, verifies the sha256, and still resolves `count[0] → counter.v:11.5`.
3. **Docs (fold-in at resolution):** update ADR 0021 Phase 2 / `docs/plans/rtl-source-provenance.md`
   WS-A/A2 with: abc_new flow is live, the aigpdk `read_liberty`+`purge_lib` finding,
   A2 = fetch-from-release (resolves the ADR/#162 "bundle vs fetch" open sub-decision),
   the publish-workflow mechanism. Then **delete this handoff**.
4. **On-ramp fixture check:** the committed `tests/**/*_synth.gv` fixtures were generated
   by the OLD classic-abc flow; if any on-ramp test regenerates via the new flow and
   diffs, regenerate deliberately (`--emit-synth`). Not yet checked.
5. **PR `ws-a-abc-new-provenance` → `main`.** Kept off `main` deliberately: the pinned
   URL 404s until step 1 publishes the release, so `main`'s default on-ramp must not
   carry the pin until the asset exists.

## Scratch / artifacts (ephemeral `/tmp`)

- Fork wasm (validated against): `/tmp/claude/prov-wasm/wheel/extracted/yowasp_yosys/{yosys.wasm,share}`
  (unpacked from the CI wheel `/tmp/claude/prov-wasm/wheel/*.whl`, sha256 `fecc687f…125af`).
- Test design + venvs: `/tmp/claude/synthtest/` (`counter.v`, `input.vcd`, `forkenv/`
  = fork yosys 0.63 `fd151be`, `synth_*.ys` iterations).

## References

- Plan: [`docs/plans/rtl-source-provenance.md`](../plans/rtl-source-provenance.md) (WS-A/A0/A1/A2).
- ADR: [`docs/adr/0021-behavioral-rtl-support.md`](../adr/0021-behavioral-rtl-support.md) (#162 = fetch vs bundle).
- Fork build/A0: `robtaylor/yowasp-yosys` `provenance-wasm.yml`, run `28779240456` (comb 4/4, seq2 88/88 `\src`).

---
**Resume with:** `/resume_handoff docs/handoffs/synth-abc-new-integration-handoff.md`
