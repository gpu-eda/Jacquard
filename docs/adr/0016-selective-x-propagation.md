# ADR 0016 — Selective X-propagation

**Status:** Accepted (2026-05). Extension to cosim proposed 2026-06-03 —
see [Amendment](#amendment-2026-06-03-cosim-and-io-x-sources).

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
Stages 1–6 are implemented; Stage 7 (dynamic X narrowing) is a
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

## Amendment 2026-06-03: cosim and IO X-sources

The original decision wired `--xprop` into the `sim` (static-input)
path only. The reactive `cosim` path is two-state, so JTAG-replay /
peripheral runs silently zero-init uninitialised state
([#95](https://github.com/gpu-eda/Jacquard/issues/95)). This amendment
extends selective X-propagation to `cosim`.

Two points the original design did not address, because the static
`sim` path never had to:

1. **Undriven input pads are X.** In a reactive run, peripheral models
   drive only *some* input bits each edge (clock, reset, JTAG/UART
   pins, configured constants). Every primary-input bit *not* in that
   driven set is unconnected and must be **X**, not `0`.
2. **Bidir pad input side is X when OE is deasserted.** A `bi_24t`
   pad's core-read (input) side is unknown whenever its output-enable
   (`OE`, surfaced as the `__oe` observable) is off and nothing external
   drives it. The input-side X-mask is refreshed per edge from `OE`.

So the X-source taxonomy is now four-way: uninitialised DFF,
uninitialised SRAM (both as before), **undriven input pads**, and
**bidir input-when-OE-off**. The first two are sequential power-up X;
the latter two are reactive IO X-sources specific to cosim.

The Metal kernel is already X-capable, so this is host-side reactive
plumbing (state-buffer expansion, per-edge X-mask maintenance, and an
observe-kernel output-offset fix for the doubled layout). Phasing and
risks are in [`../plans/cosim-xprop.md`](../plans/cosim-xprop.md). This
amendment is **proposed**, not yet implemented; the original
Decision/Consequences above describe the shipped `sim`-path behaviour.
