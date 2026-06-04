# Handoff — release prep / open threads

**Created:** 2026-06-04 (updated 2026-06-04 — release prep committed + backend-parity audit; release pushed to origin/main untagged; #108 reg_init filed from maintainer #102/#106 input)
**Branch:** main (release `7fed695` on origin/main — **pushed, untagged**)
**Working tree:** clean (after committing this handoff)

## Goal & next-up

**Goal:** ship **`v0.1.0`** (maintainer-provisioned Metal-first distribution,
ADR 0018) and carry the open release/distribution + backend-parity threads.

**Next-up (pick one):**
- **Pull the v0.1.0 trigger** — release prep is committed (`7fed695`); all
  that remains is `git push` + tag (see "Release prep — DONE" below). Highest
  value, unblocked.
- Start **#106** (X-barrier assertions, A) — actionable now, ~3–5 days, no
  hardware needed.
- Start **#105 Path A** (CPU cosim) — ~1–2 wk, no hardware, fixes cosim CI
  testability.

## Release prep — DONE (commit `7fed695`, local only)

`chore: release v0.1.0` is committed on `main` but **NOT pushed or tagged**
(deliberate — maintainer pulls the trigger):
- `CHANGELOG.md` rolled `[Unreleased]` → `[0.1.0] - 2026-06-04`; fresh empty
  `[Unreleased]`; intro updated (SemVer now in effect); compare/tag link refs
  fixed.
- Added the previously-missing **#95 `--xprop`** entry (it was entirely absent
  from the changelog) + a `known-limitation` note for deferred bidir tristate
  mux (#96).
- `Cargo.toml`/`Cargo.lock` already at `0.1.0`; verified build clean.

**To finish the release** (per `docs/release-process.md`, `packaging/README.md`):
1. `git push` then `git tag -a v0.1.0 -m "v0.1.0" && git push --tags`
   → triggers `release.yml` → **draft** GitHub release (Metal tarball + sha256).
2. Review/publish draft; grab the `.sha256`.
3. Open Homebrew tap PR (`gpu-eda/homebrew-tap`) with real url/version/sha256.
4. Separately: create `netlist-graph` PyPI project + trusted publisher, then
   tag `netlist-graph-v0.1.0`.

NB: gh's default repo for this checkout is **`gpu-eda/Jacquard`** (the git
remote `nvlabs`→`NVlabs/GEM` is the stale upstream). Issues/releases land on
gpu-eda/Jacquard.

## Backend-parity audit (this session) → issues #104, #105

Metal is the **reference backend**; CUDA/HIP are a **sim-only, reduced-capability**
path. Verified against the three `sim_*` entry points + `cmd_cosim` + kernels.

- **#104 — sim timing wiring on CUDA/HIP** (~2–4 days, **runner-gated for
  testing**). `--timing-report`/`--timing-summary`/`--timed` are Metal-only.
  The gap is **Rust-side only**: the unified kernel + the
  `simulate_v1_noninteractive_timed_{cuda,hip}` C wrappers already handle
  `timing_constraints`+`EventBuffer`, but `sim_cuda`/`sim_hip`
  (`jacquard.rs:1163`/`1332`) call the `simple_scan` (nullptr) wrapper and drop
  the constraints. Fix = share `sim_metal`'s post-kernel report logic + call
  the `timed` binding + alloc an EventBuffer (note: CUDA/HIP run all cycles in
  ONE cooperative launch, so events accumulate across cycles → single
  post-launch sweep, not per-cycle like Metal).
- **#105 — cosim is Metal-only** (`cmd_cosim` hard-errors off-Metal). Primary
  rationale = **developer access on non-Apple hardware**. Key finding: peripheral
  protocol models are **already CPU Rust** (`src/sim/models/`), and
  `cpu_reference.rs` already does CPU design-sim — only the design-step +
  `gpu_io_step`/`gpu_flash_model_step` kernels + the `cosim_metal.rs` driver are
  Metal-specific. Two paths, both need a backend-generic driver seam first:
  - **Path A — CPU cosim** (~1–2 wk, **do now, no hardware**). Justified as
    test-infra/oracle/contributor access, NOT perf (won't beat Verilator).
    Bonus: lets cosim regression tests run on free Linux CI (today Metal-only).
  - **Path B — CUDA/HIP GPU cosim** (~3–5 wk on the seam, runner-gated). The
    real Linux value prop; reuses Path A's seam.

## X-barrier assertions (this session) → issues #106, #107

`!$isunknown(net)` checks ("net must never be X after reset"). Central point:
X-ness lives in the simulator's **X-mask** (#95), not in synthesizable logic —
so these CANNOT reuse the synthesized-condition-bit assertion path
(`GEM_DISPLAY`/sim-control cells); they must be evaluated against the X-mask.
Scope boundary: cycle-based sampled boolean checks + `disable iff` guard only —
no temporal SVA (`##`, `|->`, `$past`).

- **#106 (A)** — spec-file-driven (`--x-assert <file>`, reuses trace-signals
  name resolution; `--x-assert` implies `--xprop`); eval = post-sim scan over
  `split_xprop_states` xmask → **works on all 3 sim backends** (sidesteps #104).
  ~3–5 days. Depends on #95 (merged). Pairs with #98 (`xroots` for X-origin).
  cosim x-assert is Metal-only until #105.
- **#107 (B)** — RTL SVA-subset frontend that parses `$isunknown` properties and
  lowers to A's spec. **Depends on #106.** Weeks (new SVA parser surface;
  net-identity-through-synthesis is fragile). Deferrable.

## X value-injection (maintainer input on #102/#106) → issue #108

Maintainer root-caused **#102** (JTAG-DM firmware load writes no SRAM under
`--xprop`): the X is NOT the SRAM read-mask. It originates at the **unreset CDC
launch registers** `hazard3_apb_async_bridge.src_paddr_pwdata_pwrite` (write) /
`dst_prdata_pslverr` (read) — unreset *by design* (req/ack handshake guarantees
data-stable-before-capture). Conservative X-prop poisons them; two-state + real
silicon load fine. Same X-source class as `tests/xprop_cosim/xprop_demo.v`'s
`q_unreset`.

The fix is a primitive that didn't exist → filed **#108**: register
value-injection (`reg_init` map in `TestbenchConfig`, applied at t0; the
register sibling of `sram_preload` #80/#81). **`$deposit` semantics, NOT `force`**
— force pins CDC crossing-data to 0 for the whole run (loads zeros); deposit
clears only power-up X, then the protocol drives real values.

- **#108 is the injection half; #106 (`--x-assert`) is the detection half** —
  they compose (inject at source, assert `!$isunknown` at sink = closed-loop
  barrier confirmation). Shared repro: minimal `hazard3_jtag_dtm` + `hazard3_dm`
  bench (no CPU).
- **#108 vs #103**: complementary, not duplicate. #103 preloads firmware into
  SRAM to skip the debug load entirely; #108 fixes the debug-load path itself.
  Either unblocks X-aware verification of debug-loaded firmware.
- Cosim-side `reg_init` inherits the Metal-only constraint until #105.

## Open threads (carried; re-verify before acting)

- **`release-process.md` doc inconsistency** — step 1 + the pre-release
  checklist still say "verify all three GPU backends green", contradicting
  ADR 0018's sanctioned Metal-first rollout. Now well-documented by #104/#105;
  reconcile when convenient (was deliberately left untouched this session).
- **`vendor/eda-infra-rs` bump** — RE-VERIFIED 2026-06-04: our submodule is
  already at upstream `origin/master` HEAD (`e4e3db0`), but upstream's
  `sverilogparse/Cargo.toml` STILL declares `AGPL-3.0-only` — the maintainer's
  acknowledged typo fix has NOT been pushed. Dead-end; license posture already
  fine (workspace Apache-2.0, documented in `NOTICE`). Not release-blocking.
- **NVIDIA runner won't POST** — hardware (missing CPU EPS power cable, on
  order). Once up: unblocks #104 testing, #105 Path B, CUDA/HIP CI
  (`ci.yml` jobs currently `if: ${{ false }}`), distribution Phase 4 (CUDA/HIP
  release rows).

## Deferred #95 follow-ups (not blocking)

- **#96** — bidir tristate-mux read `Y = OE ? A : external` (today bidir reads
  fall out as conservative-X).
- **Multi-macro SRAM X-mask** — single-macro 1-bit write-clears-X verified;
  16-macro/8-bit byte-mask path (test-tapeout-1) not directly exercised. (See
  also #102, #103 — cosim multi-SRAM preload + JTAG-DM debug-load X.)

## References

- Release: ADR 0018, `packaging/README.md`, `docs/release-process.md`,
  `docs/plans/distribution.md`
- X-prop: ADR 0016, `docs/plans/cosim-xprop.md`, merged PR #97 → `296cc12`
- Issues filed: #104 (CUDA/HIP sim timing), #105 (cosim portability),
  #106 (x-assert spec), #107 (x-assert SVA frontend), #108 (reg_init
  value-injection — fixes #102, pairs with #106)
- Related open: #95 (done), #96, #98 (xroots), #102, #103
