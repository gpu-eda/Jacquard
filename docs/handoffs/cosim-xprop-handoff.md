# Handoff — Selective X-propagation in cosim (#95)

**Created:** 2026-06-03
**Branch:** main
**Working tree:** clean (all WIP committed; see `e95f7c2`)

## Goal & next-up

**Goal:** Extend selective X-propagation (ADR 0016, `--xprop`) to the
reactive `cosim` path so uninitialised DFF/SRAM and undriven input pads
read as `x` instead of silently `0`. Plan: [`../plans/cosim-xprop.md`](../plans/cosim-xprop.md).

**Next session picks up at the BUG below** — X is wired but not yet
surfacing. Isolate it, then finish phases 3/4/6.

## Done

| Commit | What |
|---|---|
| `1ba01eb` | **Phases 1–2** (verified green): `--xprop` on `CosimArgs` → design build; X-mask seeded via `expand_states_for_xprop`; SRAM X-mask shadow (`0xFFFF_FFFF`) bound at `simulate_v1_stage` buffer(7) through the dispatch chain. Two-state path unchanged. Kernel needed **no** change (X-capable already; `xmask_state_offset` baked into the script at flatten time; `state_size` already uses `effective_state_size`). |
| `5255f1c` | Plan + ADR-0016 amendment; bidir read-back deferred to **#96** (folds into the generic undriven-input→X rule; conservative-safe). |
| `e95f7c2` | **Phase 5 WIP** (X NOT yet surfacing — see bug): cosim output-VCD emits `Value::X` from the X-mask half; `tests/xprop_cosim/` unreset-counter demo. Safe (behind `--xprop`, two-state green). |

Also: **#90 closed** (multi-UART, done earlier); **#96 filed** (bidir tristate-mux modelling, the proper `Y = OE ? A : external`).

## THE BUG (start here)

`tests/xprop_cosim/` has an **unreset 4-bit counter** (`count <= count + 1`;
`X + 1 = X`, so it must stay X forever) plus a reset toggling FF. Under
`--xprop`, `q_unreset` (count[0]) should show `x`; instead it toggles
`0/1` and **no `x` appears anywhere in the VCD** — even though the log
says the partition is **X-capable AND X-aware** and the X-mask is seeded.

Repro:
```sh
( cd tests/xprop_cosim && yosys -q -s synth.tcl )   # if regenerating
cargo run -r --features metal --bin jacquard -- cosim \
  tests/xprop_cosim/xprop_demo_synth.gv \
  --config tests/xprop_cosim/sim_config.json \
  --top-module xprop_demo --max-clock-edges 40 \
  --xprop --output-vcd /tmp/xdemo.vcd
grep -E '^x' /tmp/xdemo.vcd   # currently empty; should have x on q_unreset (!)
```

**Three suspects, not yet isolated:**
1. **X-mask not carried across the cosim output→input feedback** — the
   per-tick double-buffer may move only the value half, so computed
   X-masks are dropped between edges. Most likely; specific to the
   reactive loop (the `sim` path has no such feedback). Check how the
   output slot becomes the next input slot in `run_cosim` and whether the
   xmask half (`[rio..2*rio)` of each slot) is included.
2. **Phase-5 VCD xmask read offset** — `run_cosim` reads
   `output_state[rio + (pos>>5)]` for the xmask bit; verify `rio`
   (= `reg_io_state_size`) is the right per-slot offset and that the
   output slot's xmask half is where expected.
3. **Seed not consumed on edge 0** — confirm `state_prep` doesn't clobber
   the seeded input-slot xmask before the first `simulate`.

**Decisive next step:** after one edge, dump the output-slot xmask words
(`states[state_size + rio .. state_size + 2*rio]`) and check whether the
GPU produced any X bit at all. Non-zero ⇒ suspect 2 (VCD read). All-zero
⇒ suspect 1/3 (GPU not propagating/seeding). That one check splits the
tree.

(Aside: cosim `--check-with-cpu` under `--xprop` shows mismatches, but the
cosim CPU reference is not X-aware — see plan; don't trust it as a
parity oracle yet.)

## Remaining phases (after the bug)

- **Phase 3** — re-mark *undriven* primary inputs as X (driven set =
  clock/reset/model `driven_positions()`/constants stays known), with
  per-edge maintenance (likely teach `state_prep` to clear the X-mask for
  bits it drives). Bidir reads fall out of this rule as X (safe); the
  correct `Y = OE ? A : external` is **#96**.
- **Phase 4** — believed safe (value-at-front of each slot, so observe
  kernels' `states[state_size + pos]` reads hit the value half). Add a
  `--xprop` test that confirms the bus-trace/UART output is still correct.
- **Phase 6** — once X surfaces, assert the demo's `q_unreset == x`,
  `q_reset` resolves; wire into CI.

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
