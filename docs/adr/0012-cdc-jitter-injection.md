# ADR 0012 — Reproducible CDC jitter injection for multi-clock cosim

**Status:** Accepted — design accepted and partially implemented. The
reproducibility core (§1) and scheduler-domain jitter on the VCD
timeline (§2, partial) are built; model-driven jitter (§3), setup/hold
integration (§5), the `gcd_ps/2` guard, and true coincident-edge
perturbation (§4) are not yet. The sections below describe the **decided
design**; see [Implementation status](#implementation-status) for what is
built versus deferred. Remaining work is tracked in
[issue #92](https://github.com/gpu-eda/Jacquard/issues/92) and
[`../plans/cdc-jitter-completion.md`](../plans/cdc-jitter-completion.md).

> **Amendment (2026-06-25):** The Implementation status table cites
> `cosim_metal.rs` for the scheduler and jitter draw — that file reference
> is stale. The `MultiClockScheduler`, per-domain PRNG setup, and jitter
> draw loop all live in `src/sim/cosim/mod.rs` (the unified multi-backend
> cosim entrypoint); `src/sim/cosim/metal.rs` holds only the `MetalBackend`
> struct. The implemented/deferred split itself is still accurate.

## Context

The multi-clock scheduler (`MultiClockScheduler` in `cosim_metal.rs`)
pre-computes a fixed LCM-based edge schedule: every clock domain fires
at perfectly rational offsets forever. Real hardware doesn't do that —
PLL jitter, clock-tree skew, and propagation delay make coincident
edges land in unpredictable order. CDC synchronizers are designed to
tolerate this, but RTL bugs (missing synchronizers, gray-code errors,
handshake protocol violations) only surface when edge alignment varies
from the ideal.

The motivating incident was
[PR #89 / run 26413667030](https://github.com/gpu-eda/Jacquard/actions/runs/26413667030):
a scheduler index bug caused sys_clk to fire at TCK's period, making
CDC synchronizers between the JTAG and system clock domains marginal.
The test passed intermittently because Metal GPU scheduling jitter
shifted the effective phase relationship. Once the bug was fixed
(commit `5bb07c3`), determinism was restored — but the experience
highlighted that **no deliberate mechanism exists to stress-test CDC
paths under controlled timing skew**.

Additionally, cosim's model-driven clocks (`JtagReplayModel`,
`SpiFlashModel`, etc.) override the scheduler's periodic pattern with
software-driven edges. These introduce a distinct CDC concern:
model-driven clock transitions are phase-locked to the host-side
dispatch loop, not to the design's system clock. The same jitter
injection infrastructure must cover both scheduler-derived and
model-driven clock edges.

The multi-clock plan (`docs/plans/multi-clock-and-stimulus-architecture.md`)
lists "CDC verification mode: jitter injection on coincident edges and
random X-injection on detected async-source paths" as a future
capability. This ADR formalises the design for the jitter injection
half; X-injection is deferred to a follow-up ADR that depends on MC.1
(island partitioner) landing.

## Decision

### 1. Run-parameters file and per-domain seeded PRNG

Simulation runs that use any non-deterministic feature (jitter, future
partition randomisation, model-driven timing offsets) are governed by a
**run-parameters file** (`--run-params <path>`):

```json
{
  "master_seed": 8429173640281
}
```

From the master seed, a **per-domain sub-seed** is derived for each
clock domain and each model-driven clock (e.g.
`sub_seed = hash(master_seed, domain_name)`). Each domain gets its own
independent PRNG stream. This ensures reproducibility even when the
number of PRNG draws per domain is path-dependent — a reactive model
that fires more or fewer edges based on design output doesn't
contaminate another domain's displacement sequence.

**Behaviour:**

- `--run-params <path>` supplied, file exists: load parameters from it.
  The run is a deterministic replay.
- `--run-params <path>` supplied, file does not exist: generate a
  master seed from system entropy, **write the file immediately**
  (before the simulation loop starts), then run. The user gets
  reproducibility even if the process crashes mid-simulation.
- No `--run-params` flag: generate a master seed, write to a default
  location (`<output_dir>/run_params.json` next to the output VCD)
  **before simulation begins**. Always persisted — the user can
  re-run any simulation by passing the written file back.

The master seed is also logged at INFO level and included in the VCD
header comment, so even without the file the seed is recoverable from
logs.

Rationale: "random testing that can't be replayed isn't testing," but
forcing users to pick seeds upfront discourages use. Writing the file
before simulation means every run — even a crashed one — is
reproducible after the fact. Per-domain streams mean the seed alone is
sufficient; no displacement log is needed.

For CI seed sweeps, a wrapper generates N parameter files with
sequential seeds and fans out runs. Each failure ships with its
parameter file as an artifact — `gh run download` gives you
everything needed to reproduce locally.

### 2. Per-domain jitter budget

A new `jitter_ps` field on `ClockConfig` in `sim_config.json` declares
the maximum edge displacement in picoseconds for that domain:

```json
{
  "clocks": [
    { "gpio": 0, "period_ps": 40000, "name": "sys_clk", "jitter_ps": 200 },
    { "gpio": 2, "period_ps": 160000, "name": "tck", "jitter_ps": 0 }
  ]
}
```

At each edge, the scheduler draws a signed displacement from a uniform
distribution `[-jitter_ps, +jitter_ps]` and shifts the edge forward or
backward within the GCD granularity window. The resulting edge still
fires within the same GCD tick (no reordering across ticks), but the
*effective* arrival time recorded in the state buffer (and honoured by
setup/hold checkers) shifts. Disabling jitter (`jitter_ps: 0`) is the
default and produces today's ideal-clock behaviour.

Constraint: jitter must not exceed `gcd_ps / 2`; larger values would
re-order edges across GCD ticks and require a fundamentally different
scheduling model.

### 3. Model-driven clock jitter

Model-driven clocks (JTAG TCK, SPI SCK, etc.) bypass the scheduler's
periodic edges. Their jitter path is different:

- A `--cdc-model-jitter-ps <N>` flag (or per-model `jitter_ps` in the
  config) specifies the budget for model-driven transitions.
- After `patch_model_clock_edges` fires the edge, the arrival-time
  offset recorded in the timing state is displaced by a PRNG-drawn
  value from the same seeded generator.
- This does NOT delay the functional edge (the DFF still samples on
  the same tick) — it shifts the *timing-model* arrival so that
  setup/hold checks against the receiving domain see a different
  margin each run.

The functional-vs-timing split means jitter injection doesn't change
combinational propagation (which would require an event-driven kernel),
only the timing oracle's view of when edges "really" arrived. This is
consistent with Jacquard's philosophy: functional correctness is
cycle-accurate, timing is an overlay.

### 4. Coincident-edge perturbation

When two domains have edges scheduled at the same GCD tick (coincident
edges), their relative order is undefined in real hardware. The jitter
mechanism naturally handles this: if domain A's jitter shifts it
+100ps and domain B's shifts it -50ps, the timing model sees A after
B, which may differ from the next run's draw. This exercises both
"A-before-B" and "B-before-A" orderings over a seed sweep without
needing explicit permutation logic.

### 5. Integration with existing infrastructure

- **Setup/hold checker** (`timing_report.rs`): already receives
  arrival-time offsets. Jittered arrivals feed directly into the
  existing violation detection — a jitter-induced setup violation
  appears in `--timing-report` output with the jittered arrival
  annotated.
- **VCD ring buffer**: records the jittered arrival time so waveform
  viewers show the displaced edge.
- **X-prop** (future): when MC.1 identifies CDC boundaries, X-injection
  on violated paths can use the same PRNG stream for correlated
  randomisation.
- **`--check-with-cpu`**: the CPU baseline does NOT apply jitter
  (it doesn't model timing at all). Jitter-mode results should not be
  compared against the CPU baseline. The flag combination
  `--run-params` (with jitter enabled) + `--check-with-cpu` should
  warn or error.

## Implementation status

The design above is accepted in full; the code implements part of it.
This section is the source of truth on what is built. Remaining items are
tracked in [issue #92](https://github.com/gpu-eda/Jacquard/issues/92) /
[`../plans/cdc-jitter-completion.md`](../plans/cdc-jitter-completion.md).

**Implemented:**

| Part | Where |
|------|-------|
| Run-parameters file, `master_seed`, load/write/`load_or_generate` | `src/sim/run_params.rs` |
| Per-domain sub-seed `hash(master_seed, name)` + per-domain `ChaCha8Rng` | `RunParams::domain_seed`; `cosim_metal.rs` |
| `jitter_ps` per `ClockConfig` (default 0) | `src/testbench.rs` |
| Uniform `[-jitter_ps, +jitter_ps]` draw per domain per tick | `cosim_metal.rs` |
| Jitter displacement applied to the **timing-VCD event timestamp** | `cosim_metal.rs` (inside the `--output-vcd` block) |
| `master_seed` logged at INFO | `cosim_metal.rs` |
| `--check-with-cpu` + jitter warning | `cosim_metal.rs` |

**Deferred (issue #92):**

| Part | ADR § | Gap |
|------|-------|-----|
| Setup/hold integration | §2, §5 | Jitter shifts only the VCD base timestamp; it does not feed the per-signal arrival offsets, so it produces no `--timing-report` violations. Also: jitter currently has **no effect unless `--output-vcd` is set**. |
| Model-driven clock jitter | §3 | No `--cdc-model-jitter-ps` flag or `patch_model_clock_edges` path; only scheduler domains jitter. |
| True coincident-edge perturbation | §4 | A single global displacement (last firing domain wins) is applied to the shared timestamp rather than independent per-domain displacement. |
| `gcd_ps / 2` constraint | §2 | Not validated. |
| Persist seed unconditionally | §1 | Without `--run-params` or `--output-vcd`, the seed is generated but not written. |
| `master_seed` in VCD header comment | §1, §5 | INFO log only. |
| `--cdc-jitter-seed` CI sweep | Consequences | The replay mechanism is `--run-params`; no dedicated CI sweep step yet. |

## Consequences

- CI can run a small seed sweep (via `--run-params`) as a lightweight
  CDC stress test on every PR, catching synchroniser failures that the
  ideal-clock schedule hides.
- Users debugging real silicon CDC failures can replay the exact jitter
  pattern that triggered the issue.
- The design is forward-compatible with X-injection (the PRNG
  infrastructure and per-domain budgets are reusable).
- Model-driven clocks get explicit jitter coverage rather than relying
  on accidental GPU scheduling delays.
- No kernel changes required — jitter is a host-side timing-model
  overlay on the existing edge schedule.

## Deferred

- **X-injection on CDC paths.** Requires MC.1's island partitioner to
  identify which DFF outputs cross domains. Separate ADR once MC.1
  lands.
- **Frequency sweep / DFS simulation.** Changing a clock's period
  mid-simulation is orthogonal to jitter. Captured in the multi-clock
  plan as a future axis.
- **Per-path jitter profiles.** Real jitter isn't uniform — PLLs have
  period jitter (Gaussian), recovered clocks have cycle-to-cycle jitter
  (bounded), external clocks have frequency offset (deterministic
  drift). V1 uses uniform; richer distributions can be added later
  without API changes (the seed + budget interface is distribution-agnostic).
