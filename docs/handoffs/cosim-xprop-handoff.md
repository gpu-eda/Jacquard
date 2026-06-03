# Handoff — Selective X-propagation in cosim (#95)

**Created:** 2026-06-03
**Branch:** main
**Working tree:** clean (all WIP committed; see `e95f7c2`)

## Goal & next-up

**Goal:** Extend selective X-propagation (ADR 0016, `--xprop`) to the
reactive `cosim` path so uninitialised DFF/SRAM and undriven input pads
read as `x` instead of silently `0`. Plan: [`../plans/cosim-xprop.md`](../plans/cosim-xprop.md).

**The "X never surfaces" bug is now FIXED** (root cause was *not* any of
the three suspects below — see "Resolved" section). Remaining work is
phases 3/4/6.

## Done

| Commit | What |
|---|---|
| `1ba01eb` | **Phases 1–2** (verified green): `--xprop` on `CosimArgs` → design build; X-mask seeded via `expand_states_for_xprop`; SRAM X-mask shadow (`0xFFFF_FFFF`) bound at `simulate_v1_stage` buffer(7) through the dispatch chain. Two-state path unchanged. Kernel needed **no** change (X-capable already; `xmask_state_offset` baked into the script at flatten time; `state_size` already uses `effective_state_size`). |
| `5255f1c` | Plan + ADR-0016 amendment; bidir read-back deferred to **#96** (folds into the generic undriven-input→X rule; conservative-safe). |
| `e95f7c2` | **Phase 5 WIP** (X NOT yet surfacing — see bug): cosim output-VCD emits `Value::X` from the X-mask half; `tests/xprop_cosim/` unreset-counter demo. Safe (behind `--xprop`, two-state green). |
| *(uncommitted)* | **Core seed fix** (see "Resolved" below): `vcd_io::xprop_xmask_template` seeds X at genuine X-sources only (not all `input_map`); `run_cosim` seeds the output-slot X-mask too. X now surfaces in **both** sim and cosim. Tests updated; 291/291 green. |

Also: **#90 closed** (multi-UART, done earlier); **#96 filed** (bidir tristate-mux modelling, the proper `Y = OE ? A : external`).

## Resolved: the real root cause (2026-06-03)

The "X never surfaces" symptom was **not** cosim-specific and **not** any
of the three suspects originally listed (carry-across-feedback / VCD read
offset / edge-0 clobber were all investigated and ruled out — the kernel,
the `state_prep` copy of the full `effective_state_size`, and the VCD read
at `output_state[rio + w]` are all correct).

**Actual bug — a core seed-template error affecting BOTH `sim` and
`cosim`:** `expand_states_for_xprop` built the power-up X-mask as *"all-X,
then clear every `input_map` position."* But `input_map` includes the
**DFF-Q feedback-read positions**, not just primary input ports. So every
uninitialised DFF was read by combinational logic as **known 0**, and X
never originated. `--xprop` was silently two-state for any sequential
design. It went unnoticed because the only xprop tests were gate-level AND
unit tests plus a fixture that (wrongly) modelled DFF Q outside
`input_map`.

Isolation path (kept for the record): seeded input slot had the X (verified
`IN xmask=1` at the DFF Q), but the kernel wrote known `0` — because the
*global read* never loaded any X into `shared_state_x`: every bit it
consumed had been cleared in the template (the DFF-Q read positions). The
CPU reference (`simulate_block_v1_xprop`) agreed bit-for-bit with the GPU,
proving the bug was algorithmic, not GPU-specific.

**Fix:**
- `vcd_io::xprop_xmask_template(script)` — new shared helper. Builds the
  template as *"all-known, set X only at genuine X-source positions"*
  (uninitialised DFF Q reads + SRAM reads), excluding primary inputs (nets
  in `input_layout`) and constant-pinned DFFs (`const_zero_pos =
  input_layout.len()`). Used by `expand_states_for_xprop` and the sim
  `--check-with-cpu` path.
- `run_cosim` also seeds the **output** slot's X-mask half (not just the
  input slot): cosim's per-edge `state_prep` copies output→input first, so
  the seed must be in the output slot to survive edge 0.

**Verified:** sim `--xprop --check-with-cpu` → `q_unreset=x` (persists),
`q_reset` resolves, CPU↔GPU agree; cosim `--xprop` → same; two-state
unchanged; mcu_soc cosim `--xprop` runs clean (X resolves through reset,
28 X-transitions, not drowning); 291/291 lib tests pass.

Repro (now shows `x` on `q_unreset`):
```sh
cargo run -r --features metal --bin jacquard -- cosim \
  tests/xprop_cosim/xprop_demo_synth.gv \
  --config tests/xprop_cosim/sim_config.json \
  --top-module xprop_demo --max-clock-edges 40 \
  --xprop --output-vcd /tmp/xdemo.vcd
grep -E '^x' /tmp/xdemo.vcd   # now non-empty
```

(Aside: cosim `--check-with-cpu` under `--xprop` still shows mismatches —
the cosim CPU reference is not X-aware; don't trust it as a parity oracle.
The *sim* `--check-with-cpu` xprop path IS X-aware and passes.)

## Remaining phases (after the bug)

- **Phase 3 — DONE.** Undriven primary inputs read X:
  `compute_x_capable_pins(treat_inputs_as_x_sources)` (gated by
  `DesignArgs::xprop_undriven_inputs`, true for cosim only) makes input
  cones X-capable; `xprop_xmask_template_cosim` seeds all primary inputs X;
  the driven set is exactly what `build_edge_ops` emits, and `state_prep`
  clears the X-mask of each bit it drives (driven ⇒ known) so undriven ones
  stay X. **`gpu_apply_flash_din` also clears the X-mask** of the flash
  MISO bits it drives (they bypass `state_prep`) — without this the SPI
  read drowns in X. Bidir reads fall out as X (safe); the correct
  `Y = OE ? A : external` is **#96**. Verified: demo `q_undriven_reg`/`comb`
  read X under cosim `--xprop`; mcu_soc boots (flash `0x03` read decoded).
- **Phase 4** — believed safe (value-at-front of each slot, so observe
  kernels' `states[state_size + pos]` reads hit the value half). Add a
  `--xprop` test that confirms the bus-trace/UART output is still correct.
  **Still open** — the only remaining phase.
- **Phase 6 — DONE.** `tests/xprop_cosim/` is an end-to-end guard for both
  sim and cosim: `check.py` (modes `xprop` / `xprop-cosim` / `two-state`)
  asserts `q_unreset==x`, `q_reset` resolves, and the phase-3 undriven
  outputs; wired into the Metal CI job (fatal). The existing dff_test
  xprop check was also made fatal.

## Other open threads (from the prior release-prep handoff)

- **NVIDIA runner won't POST** — hardware (missing CPU EPS power cable,
  cable on order). Once up: distribution **Phase 4** (CUDA/HIP release
  rows in `release.yml`) + re-enable CUDA CI (`ci.yml` `if: ${{ false }}`)
  + CUDA/HIP timing-report routing (`process_events`/`ReportingCtx`).
- **`vendor/eda-infra-rs` bump** — blocked on upstream sverilogparse
  license-string fix; `git -C vendor/eda-infra-rs fetch && git show
  origin/master:sverilogparse/Cargo.toml | grep license`.
- **Maintainer-provisioned distribution** (ADR 0018): cut `v0.1.0` (→
  draft release + tap formula PR), create the `netlist-graph` PyPI
  project + trusted publisher. See `packaging/README.md`.

## References

- Plan: [`../plans/cosim-xprop.md`](../plans/cosim-xprop.md)
- ADR: [`../adr/0016-selective-x-propagation.md`](../adr/0016-selective-x-propagation.md) (amendment)
- Issues: #95 (this), #96 (bidir tristate mux)
- Sim-path xprop to mirror: `vcd_io::{expand_states_for_xprop,
  split_xprop_states, write_output_vcd_xprop}`; `sim_metal` in
  `src/bin/jacquard.rs` (~line 752).
