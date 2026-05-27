# ADR 0017 — Cosim execution model

**Status:** Accepted

## Context

The cosim mode runs a GPU-simulated design alongside reactive
peripheral models (flash, UART, JTAG, GPIO) that drive and observe
design pins each clock edge. The execution model must balance two
competing needs: GPU throughput (which favours large batches of
edges dispatched as a single command buffer) and peripheral
responsiveness (which requires CPU-side model updates between edges).

This ADR documents the batch dispatch loop, the multi-clock
scheduler, and the time-domain abstractions that tie them together.

## Decision

### Batch dispatch loop

The cosim main loop groups consecutive scheduler edges into
**batches** of up to `BATCH_SIZE = 1024` edges. Each batch is
encoded into a single Metal command buffer and dispatched to the
GPU. Between batches, CPU-side peripheral models (`PeripheralModel::
step_edge`) run, ring buffers are drained, and model overrides are
compiled into `BitOp` arrays for the next batch.

Per-edge execution within a batch:

```
state_prep (apply clk/gpio/jtag pin drives via BitOps)
  → gpu_apply_flash_din (inject flash MISO into input state)
    → simulate_v1_stage ×N (combinational logic evaluation)
  → gpu_flash_model_step (read MOSI, advance flash FSM)
  → gpu_io_step (UART TX decode + Wishbone bus trace)
```

CPU-side models cannot observe intra-batch state changes — they see
the output state only after the batch completes. For peripherals
that require per-edge responsiveness (e.g. JTAG replay with tight
hold-cycle requirements), the batch is forced to size 1 when any
model reports `is_active() == true`.

### Why BATCH_SIZE = 1024

The batch size trades off GPU utilisation against peripheral
latency. Smaller batches → more Metal command buffer submissions
per second → higher overhead. Larger batches → staler CPU-side
model state. 1024 was chosen empirically as a sweet spot:

- For peripheral-free simulation: amortises ~1ms of command buffer
  overhead across 1024 edges ≈ 1µs/edge overhead.
- For active peripherals (JTAG, stimulus-driven): the `is_active`
  fallback to batch=1 ensures correctness regardless of batch size.
- The batch size only affects cosim; the `sim` command processes the
  entire VCD in one GPU dispatch.

### Pre-allocated schedule buffers

Each scheduler edge has pre-allocated Metal buffers for its
`StatePrepParams` and `BitOp` array (`ScheduleBuffers::edge_buffers`).
These are allocated once at startup — not per-dispatch — to avoid
allocation latency in the hot loop. The schedule repeats with period
`edges_per_period` (= LCM schedule length); edge `N` reuses
buffer `N % edges_per_period`.

### Multi-clock scheduler

The `MultiClockScheduler` computes a deterministic interleaving of
edges across clock domains. Given N clocks with potentially
different periods and phase offsets:

1. Compute `gcd_ps` = GCD of all half-periods and phase offsets.
   This is the **scheduler tick** — the minimum time quantum.
2. Compute `lcm_ps` = LCM of all full periods. This is the
   **schedule period** — the point at which the edge pattern repeats.
3. `schedule_len = lcm_ps / gcd_ps` — number of ticks per period.
4. For each tick, compute which domains have rising/falling edges
   based on `(tick_ps - phase_offset) % half_period == 0`.

The schedule length is capped at 1,000,000 ticks. This prevents
degenerate clock ratios (e.g. primes) from producing unbounded
schedules. If the cap is hit, the assertion fires with a message
suggesting the clocks may not be commensurable at the configured
resolution.

### Time units: edges vs clock cycles

A **scheduler edge** is one tick of the scheduler (duration =
`gcd_ps`). A **clock cycle** is two half-periods of a given domain
(= rising + falling edge). The ratio `edges_per_sys_clk_cycle =
clock_period_ps / gcd_ps` converts between them.

This distinction is load-bearing for peripheral timing:
- UART baud rate dividers count **edges**, not clock cycles.
- Reset duration counts **edges**.
- The `--max-clock-edges` CLI flag counts **edges**.

Confusing edges with clock cycles was the root cause of the UART
baud rate bug fixed in commit `a263e47` — `edges_per_period` (the
LCM schedule length) was used where `edges_per_sys_clk_cycle` was
needed, doubling the bit time in multi-clock designs.

### GPU→CPU ring buffer drain

After each batch completes, the CPU drains three categories of
GPU-side state:

1. **Peripheral ring buffers** — UART channels and Wishbone trace
   channel, drained from local `read_head` to GPU-written
   `write_head`. See ADR 0013 for struct conventions.
2. **VCD snapshot buffer** — when `--stimulus-vcd` or `--timing-vcd` is
   enabled, a separate ring buffer (`2 × state_size` words per edge)
   captures per-tick output state on the GPU. The CPU drains it
   after each batch to write VCD transitions. This mechanism is what
   enables `BATCH_SIZE > 1` even with VCD output — without it, the
   CPU would need to read output state after every single edge.
3. **CPU reference check** — when `--check-with-cpu` is active, the
   CPU replays the batch with the reference kernel and compares.

No synchronisation beyond Metal's command buffer completion is
needed — all drains happen after `waitUntilCompleted`.

## Consequences

- The batch dispatch model means CPU-side peripheral models see
  output state with up to `BATCH_SIZE` edges of latency. This is
  acceptable for all current peripherals; models that need tighter
  coupling set `is_active() = true`.
- The 1M tick schedule cap prevents pathological memory use but
  rejects exotic clock ratios. A min-heap scheduler (proposed in
  `docs/plans/multi-clock-and-stimulus-architecture.md` as MC.2)
  would remove this limit.
- The edges-vs-cycles distinction must be maintained carefully in
  any code that converts user-facing "cycles" to internal "ticks".
  The `edges_per_sys_clk_cycle` helper exists for this purpose.
- Pre-allocated schedule buffers consume `O(schedule_len)` Metal
  buffer pairs at startup. Each schedule entry creates two Metal
  buffer objects (params + ops). For typical single-clock designs
  this is 2 entries = 4 buffer objects; for complex multi-clock
  designs it can reach thousands of entries, but each buffer is
  small (tens of bytes).

## Cross-references

- ADR 0012 — CDC jitter injection (uses the scheduler's edge
  timestamps as the injection point).
- ADR 0013 — Peripheral model architecture (documents GPU-side
  model patterns and ring buffers).
- [`docs/plans/multi-clock-and-stimulus-architecture.md`](../plans/multi-clock-and-stimulus-architecture.md)
  — design-space doc for the multi-clock scheduler.
