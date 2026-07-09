# Handoff — C64 cosim blockers: derived clock (#185) + SRAM macro (#186)

**Branch:** `feat/derived-clock-divider` (off `main` @ `346b0169`) holds the #185
work. #186 is unstarted (branch from `main`).
**Issues:** gpu-eda/Jacquard#185, #186.

## Goal & next-up

**Goal:** get the C64 (`chip_top`) post-PnR netlist through `jacquard cosim` so
the video-streaming tap (#184) can render the VIC output. Two Jacquard bugs block
it; the video tap itself is ready.

**Next session picks up, in order:** (1) finish **#185** on this branch, (2)
tackle **#186** (fresh branch from main), (3) #184 video-tap e2e once both clear.

## Context (what's already landed / ready)

- **#183 QSPI shared-bus CS-gate — MERGED to `main`** (`346b0169`). Shared SCK/SIO
  + distinct CS works up to `MAX_QSPI_MEMS = 4`. (Answered the C64 session's
  earlier questions: shared-bus QSPI works now; bind flash pins to top-level
  `bidir_PAD` ports — the `bi_24t` split yields the `CORE2PAD`/`PAD2CORE`
  core-boundary signals; no internal-net binding needed.)
- **#184 video tap — draft**, branch `feat/cosim-signal-stream-tap` (rebased on
  main). Feature + C64 renderer/config in `docs/spikes/video-streaming-tap/`.
  Verified net names: strobe `\i_chip_core.c64_u.enablepixel`, video pads
  `\bidir_CORE2PAD[1..6]`. Ready for e2e once the netlist loads.
- Cross-session coordination is via the GitHub issues (both instances read them),
  not handoffs. Per-project git-identity plan (deferred):
  `~/Code/Claude/claude-git-identity-plan.md`.

## Next step 1 — #185 derived (on-die divided) clock (medium)

The C64's `clk32` is an on-die `clk_pad ÷ 2` toggle-DFF divider; the AIG clock
tracer panics (`aig.rs:2676`) on the divider FF. **Direction 2**: config declares
the derived clock; AIG cuts the divider; scheduler drives it as a commensurable
÷N domain (mirrors STA `create_generated_clock`).

**Done on this branch:**
- `2968f96d` — `tests/derived_clock/` minimal ÷2 repro (toggle DFF → counter),
  reproduces the exact panic in a tiny design.
- `27d45749` — `ClockConfig` schema: `gpio`/`period_ps` optional; derived clock
  `{ net, derived_from, divide_by }` + `is_derived()`. Backward-compatible; no
  behavior change (multi_mem verified). Scheduler-timing matching skips derived
  entries with a "not yet implemented" warn.

**Validated design (load-bearing):**
- `MultiClockScheduler::new` (`mod.rs:1482`) already interleaves a commensurable
  ÷N via gcd/lcm of half-periods.
- `build_edge_ops` (`mod.rs:1584-1621`) drives domains by **flag bits** with
  `clock_input_pos` **optional** — so a derived clock needs no primary-input
  value slot; the cut's `InputClockFlag` flags suffice.
- The tracer already mints `InputClockFlag` at undriven nets (`aig.rs:872-895`);
  the cut reuses that path for a config-declared net.

**Remaining (exact sites):**
- **A. Thread net names into AIG build.** `DesignArgs` (`setup.rs:23`) +=
  `derived_clock_nets: Vec<String>`; `build_netlist_and_aig` (`setup.rs:109`,
  callers: `load_design` `setup.rs:382`, `jacquard.rs:2090`, `x_sources.rs:330,416`);
  `from_netlistdb_with_cells_and_descriptor` (`aig.rs:2531`) + `from_netlistdb_impl`
  (`aig.rs:2563`) get the param (other constructors pass `&[]`). In
  `from_netlistdb_impl`, resolve names→net ids into a new AIG field
  `derived_clock_netids: HashSet<usize>` before the clock-trace loop.
- **B. The cut** in `trace_clock_pin` (`aig.rs:868`): if the net is a
  derived-clock net, terminate + mint `InputClockFlag` on the driver pin (reuse
  `aig.rs:875-895`) instead of tracing through the divider FF. Divider FF stays as
  dead-for-clock logic that still computes the observable net value.
- **C. Scheduler binding** (`mod.rs` clock-timings ~`2367`): replace the "skip
  derived" branch — resolve `clk_cfg.net` → driver pin → matching
  `clock_domains` entry; `half_period_ps = parent_period × divide_by / 2`.
- **D. Config → DesignArgs** (`jacquard.rs` cmd_cosim ~`2277`): collect
  `effective_clocks().filter(is_derived).filter_map(net)` into
  `design_args.derived_clock_nets`.
- **E. Fixture + test**: switch `tests/derived_clock/sim_config.json` to the
  derived form (below), assert the counter increments at half rate (golden +
  `check.py`), wire into `cosim_cpu_check.sh` `all` scope.

```jsonc
"clocks": [
  { "gpio": 0, "period_ps": 15625, "name": "clk" },
  { "net": "clkdiv", "derived_from": "clk", "divide_by": 2, "name": "clkdiv" }
]
```

**Correctness:** the real divider holds `clkdiv` during reset (`RN`); a
scheduler-driven domain toggles from t=0. Post-reset is identical — add a check
the derived edges line up after reset (or gate on reset), and that the divider
FF's observable `clkdiv` matches the ÷2 phase.

**Risk:** `trace_clock_pin` gates **every** design — run the full
`cosim_cpu_check.sh all` golden suite + Metal after the cut.

**Interim unblock (no code):** manually patch the post-PnR netlist — cut the
divider, tie `clkdiv` to a new top-level clock port, declare it a normal clock at
half the pad frequency. Gets the C64 smoke test moving without the tracer change.

## Next step 2 — #186 SRAM macro offset panic (unstarted, unknown size)

`flatten.rs:1268` `Option::unwrap` on `None` for **cell-library-declared SRAM
macros**. The C64's on-die ZP/stack SRAM hits this. Not yet investigated — needs
its own scoping pass (read `flatten.rs` around 1268, find the offset walk that
assumes an SRAM property present for AIGPDK-native RAM but absent for
cell-library macros). Branch from `main`, not from the derived-clock branch.

## Next step 3 — #184 video-tap end-to-end (small, after 1+2)

Run cosim with the `signal_streams` block (`docs/spikes/video-streaming-tap/example-signal-streams.json`)
+ the pygame renderer. Confirm the two open questions: net-name resolver form
(leading `\`?) and `colorIndex` bit order. Runbook in the video-tap README.

## Housekeeping (trivial, deferred)

Stray untracked `tests/qspi_shared_bus/expected/run_params.json` (generated
seed-dump) — safe to `rm`; add `run_params.json` to `.gitignore` on main.
