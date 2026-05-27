# ADR 0016 — Selective X-propagation

**Status:** Accepted

## Context

Jacquard's default two-state (0/1) simulation silently resolves
uninitialised DFF and SRAM outputs to zero. This masks
initialisation bugs that real hardware would expose as unknown (X)
values, and creates false mismatches when comparing against
four-state RTL simulators.

Naively upgrading the entire simulator to four-state logic would
double storage and roughly halve throughput. In a well-designed SoC
after reset, typically less than 5% of signals are genuinely
X-capable.

## Decision

Implement **selective X-propagation** controlled by the `--xprop`
CLI flag. Static analysis at compile time identifies X-source
signals (uninitialised DFFs, SRAM read ports); forward-cone
computation classifies each partition as X-capable or X-free. Only
X-capable partitions run an X-aware kernel variant; the rest
continue with the fast two-state path.

The full seven-phase design, implementation details, and design
rationale are in
[`docs/selective-x-propagation.md`](../selective-x-propagation.md).
All stages are implemented (Stages 1–7); dynamic narrowing is a
future enhancement.

### Key design choices (summary)

1. **Partition-level granularity** — entire partition runs X-aware
   or not. ~95% of partitions are typically X-free after reset.
2. **Conservative SRAM X** — all reads return X until any write.
   Per-address tracking deferred.
3. **No reset-aware analysis** — all DFFs start as X; the fixpoint
   iteration naturally resolves reset-connected DFFs.
4. **State buffer doubling** — X-mask words occupy
   `[reg_io_state_size .. 2*reg_io_state_size)` when enabled.
   X-free partitions ignore the mask entirely.
5. **Runtime flag, not compile-time** — `--xprop` on `jacquard sim`;
   no new Cargo features needed.

## Consequences

- X-capable partitions pay ~2× storage and ALU cost; X-free
  partitions (the vast majority) pay nothing.
- VCD output includes `x` values when `--xprop` is enabled,
  compatible with standard four-state VCD tools.
- The `--check-with-cpu` reference path includes an X-aware CPU
  kernel for validation.
- Benchmarks (`benches/xprop.rs`) track the overhead.
