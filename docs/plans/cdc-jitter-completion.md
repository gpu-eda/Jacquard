# Plan: complete Decision 0012 CDC jitter injection

Tracks the deferred half of [Decision 0012](../architecture/decisions/0012-cdc-jitter-injection.md).
Issue: [#92](https://github.com/gpu-eda/Jacquard/issues/92).

## Where it stands

Implemented: the run-parameters file + per-domain seeded PRNG
(`src/sim/run_params.rs`), `jitter_ps` per `ClockConfig`, the uniform
per-domain draw, and a jitter displacement applied to the **timing-VCD
event timestamp** (`cosim_metal.rs`, inside the `--output-vcd` block
only). So today jitter perturbs the *waveform timeline* but nothing
else — it does not reach the setup/hold checker, model-driven clocks,
or coincident-edge ordering.

The goal of this plan is to make jitter actually stress CDC paths, then
extend it to model-driven clocks and tidy the loose ends, so Decision 0012's
present-tense design fully matches the code.

## Phase 1 — Jitter reaches the timing checker (the core value)

Right now `jitter_displacement` only adjusts the VCD `base_timestamp`
(`cosim_metal.rs:~3928-3948`) and is computed *inside* the timing-VCD
emission block, so it has no effect without `--output-vcd` and never
influences violations.

- Hoist the per-tick per-domain displacement draw out of the VCD block
  so it is available whenever `jitter_active`, independent of
  `--output-vcd`.
- Apply each domain's displacement to the **arrival offsets** that
  setup/hold checking consumes (the `arrival_state` section), not just
  the VCD base timestamp — so a jittered edge can move a margin across
  the setup/hold boundary and surface in `--timing-report`.
- **True per-domain perturbation** (ADR §4): keep a displacement *per
  firing domain* this tick rather than the current single global value
  (the loop overwrites `jitter_displacement` with the last domain's
  draw). Coincident edges from domains A and B then move independently,
  exercising both orderings over a seed sweep.

Verify: a small two-domain design with a deliberately marginal CDC path;
assert that a seed sweep produces both "no violation" and "violation"
outcomes, and that a fixed seed reproduces exactly.

## Phase 2 — Model-driven clock jitter (ADR §3)

Model-driven clocks (`JtagReplayModel`, SPI SCK, …) bypass the scheduler
and currently get no jitter.

- Add `--cdc-model-jitter-ps <N>` (and/or per-model `jitter_ps` in
  config) → a budget + seeded stream via `RunParams::domain_seed(model_name)`.
- After a model fires its edge, displace the **timing-model arrival**
  for that transition (not the functional edge — the DFF still samples
  on the same tick), mirroring the Phase 1 arrival-offset path.

Verify: extend `tests/jtag_minimal` (model-driven TCK) with a model
jitter budget; confirm reproducibility and that TCK→sys_clk CDC margins
vary by seed.

## Phase 3 — Hygiene / correctness guards

- **`gcd_ps / 2` constraint** (ADR §2): at startup, error (or clamp with
  a loud warning) if any `jitter_ps > scheduler.gcd_ps / 2`, since larger
  values would reorder edges across GCD ticks.
- **Always persist the seed** (ADR §1): when neither `--run-params` nor
  `--output-vcd` is given, `RunParams::generate()` currently does not
  write the file. Persist to a default path unconditionally so every run
  is replayable.
- **master_seed in the VCD header** (ADR §1/§5): emit the master seed as
  a VCD header comment in `vcd_io.rs`, so the seed is recoverable from an
  output artifact, not just the INFO log.

## Phase 4 — CI CDC stress sweep (ADR Consequences)

Once jitter feeds violations (Phase 1), add a lightweight CI step: run
the marginal-CDC design across a few sequential seeds, upload each run's
`run_params.json` as an artifact, fail if an unexpected violation
appears. Gives every PR a cheap CDC regression.

## Out of scope (separate ADRs / plans)

- X-injection on CDC paths (needs MC.1 island partitioner — Decision 0012
  "Deferred").
- Non-uniform jitter distributions (Gaussian period jitter, etc.) — the
  seed+budget interface is distribution-agnostic, add later.
- Frequency sweep / DFS.
