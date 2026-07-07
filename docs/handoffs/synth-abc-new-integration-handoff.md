# Handoff — WS-A integration: `synth.rs` → `abc_new` origins flow + A2 fetch

**Updated:** 2026-07-07
**Branch:** `ws-a-abc-new-provenance` (pushed; commits `42607dcc` synth+A2,
`9a29615c` handoff; local uncommitted: `publish-yosys-wasm.yml` deletion +
pending `synth.rs` re-pin).
**Status:** on-ramp done + validated; A2 hosting **redesigned** — see below.
**Parent thread:** [`rtl-source-provenance-handoff.md`](rtl-source-provenance-handoff.md).

## A2 hosting — REDESIGNED 2026-07-07 (supersedes the PAT/Jacquard-asset plan)

Earlier iterations (Jacquard release asset via local upload → blocked by GitHub's
~120 s upload cap; then a Jacquard workflow pulling the fork CI artifact → needed a
cross-repo PAT + a 14-day-expiring artifact) were **abandoned**. Final design:

- **Transferred `robtaylor/yowasp-yosys` → `gpu-eda/yowasp-yosys`** (done; public;
  robtaylor URL redirects). Nested `robtaylor/yosys`→`robtaylor/abc` + `povik/yosys-slang`
  stay under their owners (public → in-repo CI clones them anonymously, no PAT).
- **The fork's own `provenance-wasm` CI now self-publishes the release.** Added a
  gated `release` job (`gpu-eda/yowasp-yosys@yowasp-yosys-integration`, commit
  `de797e07`): on `workflow_dispatch` with a `release_tag`, after build +
  provenance-check pass, it attaches the validated wheel to a `gpu-eda/yowasp-yosys`
  release via softprops — **in-repo `GITHUB_TOKEN`, no PAT, no artifact expiry**.
  README got a fork-purpose banner.
- **jacquard pins the `gpu-eda/yowasp-yosys` release** (in-org, public). Jacquard's
  own `publish-yosys-wasm.yml` is **deleted** (the fork self-publishes now).

**IN FLIGHT:** build+publish run **`28836059973`** (tag **`wasm-de797e07`**), ~50 min.
NOTE: the 2 new fork commits bump the wheel `postNNNN` version, so the published
wheel filename + sha256 **differ from A0's** (`fecc687f…` / `post1135`) — pin what
this run actually publishes (its sha256 is in the release body + job summary).

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
  guards URL⊇TAG + sha256 format. **Pin must be updated** from the in-flight run
  (see below) — the current committed values (`yosys-wasm-0.63.0.0.post1135` /
  `fecc687f…`, gpu-eda/Jacquard URL) are STALE and point at the abandoned plan.

## Remaining steps (after run `28836059973` completes)

1. **Read the published wheel's sha256 + filename** from the `gpu-eda/yowasp-yosys`
   release `wasm-de797e07` (release body / job summary), then **re-pin `src/synth.rs`**:
   - `YOSYS_WASM_TAG = "wasm-de797e07"`
   - `YOSYS_WASM_URL = "https://github.com/gpu-eda/yowasp-yosys/releases/download/wasm-de797e07/<wheel>"`
   - `YOSYS_WASM_SHA256 = "<published sha256>"`
   - Fix the const doc comment (drop the deleted-workflow reference; point at the fork release).
2. **Commit** the batched Jacquard change: `publish-yosys-wasm.yml` deletion (already
   `git rm`'d) + the re-pin. `cargo test --features metal,synth --lib pinned_wasm_consts`.
3. **Validate the default fetch end-to-end:** clear `JACQUARD_YOSYS_WASM` + cache
   (`rm -rf ~/.cache/jacquard/wasm`), run `jacquard sim counter.v … --trace-signals`
   (fixtures in `/tmp/claude/synthtest/`) → confirm it downloads from gpu-eda/yowasp-yosys,
   verifies the sha256, and still resolves `count[0] → counter.v:11.5`.
4. **Docs (fold-in at resolution):** ADR 0021 Phase 2 / `docs/plans/rtl-source-provenance.md`
   WS-A/A2 — abc_new flow live, the aigpdk `read_liberty`+`purge_lib` finding, A2 =
   fetch a fork-self-published release (resolves ADR/#162 "bundle vs fetch"), the repo
   move. Then **delete this handoff**.
5. **On-ramp fixture check:** committed `tests/**/*_synth.gv` were generated by the OLD
   classic-abc flow; if any on-ramp test regenerates via the new flow and diffs,
   regenerate deliberately (`--emit-synth`). Not yet checked.
6. **PR `ws-a-abc-new-provenance` → `main`** once the release exists + fetch validated.

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
