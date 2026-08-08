# Decision 0016 — Selective X-propagation

**Status:** Accepted (2026-05; amended 2026-06-25). Extension to cosim

**Current architecture:** [Simulation engine](../simulation-engine.md).
proposed 2026-06-03 — see [Amendment](#amendment-2026-06-03-cosim-and-io-x-sources).

> **Amendment (2026-06-25):** Stage-count correction. The body says
> "Stages 1–6 are implemented; Stage 7 (dynamic X narrowing) is a future
> enhancement." Per `docs/selective-x-propagation.md`, **all seven stages
> are implemented** — Stage 7 is the Criterion benchmarks
> (`benches/xprop.rs`), not dynamic narrowing. *Dynamic X narrowing*
> (periodic X-mask scan, partition-kernel hot-swap) is a separate, still
> unbuilt enhancement.

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
[`docs/selective-x-propagation.md`](../../selective-x-propagation.md).
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
2. **Bidir pad reads.** A `bi_24t` pad's core-read was originally
   modelled `Y = PAD` (tristate not modelled); since `PAD` is an undriven
   primary input, bidir reads fell out of rule (1) as X — safe (false-X,
   never false-0) but pessimistic for the `OE=1` loopback. The
   combinationally-correct read `Y = OE ? A : external` is now modelled as
   a mux in the AIG ([#96](https://github.com/gpu-eda/Jacquard/issues/96),
   implemented — see the dated subsection below). (An earlier draft of this
   amendment proposed a per-edge OE→input feedback with one-edge latency —
   that was wrong; the correct read is combinational.)

So the X-source taxonomy is now three-way: uninitialised DFF,
uninitialised SRAM (both as before), and **undriven input pads** (which
subsumes bidir reads under the current `Y = PAD` model). The first two
are sequential power-up X; the third is the reactive IO X-source
specific to cosim.

The Metal kernel is already X-capable, so this is host-side reactive
plumbing (state-buffer expansion, per-edge X-mask maintenance, and an
observe-kernel output-offset fix for the doubled layout). Phasing and
risks are in [`../plans/cosim-xprop.md`](../../plans/cosim-xprop.md).

### Seed-template correction (2026-06-03)

Implementing the cosim extension surfaced a latent bug in the **shipped
`sim` path** too: the power-up X-mask seed
(`expand_states_for_xprop`) was built as "all-X, then clear every
`input_map` position." But `input_map` contains the DFF-Q
*combinational-read* positions, not just primary input ports — so
uninitialised DFFs were read as known `0` and X never originated. `--xprop`
was therefore silently two-state for **any** sequential design (the
gate-level X math was unit-tested, but no end-to-end test asserted X
surfacing from an uninitialised DFF).

The seed is now built by `vcd_io::xprop_xmask_template`: **all-known, set
X only at genuine X-source positions** (uninitialised DFF Q reads + SRAM
reads), excluding primary inputs (nets present in `input_layout`) and
constant-pinned DFFs (`const_zero_pos = input_layout.len()`). The cosim
path additionally seeds the *output* slot's X-mask, since its per-edge
`state_prep` copies output→input before the first `simulate`. This
corrects design choice #3 above ("all DFFs start as X"): the *intent* was
always X at DFF positions; the implementation had inverted it for
DFF-feedback reads.

### Undriven input X-source (cosim, implemented 2026-06-03)

The "undriven input pad → X" rule from this amendment is now implemented
for `cosim`. `compute_x_capable_pins(treat_inputs_as_x_sources)` (gated by
`DesignArgs::xprop_undriven_inputs`, set only by the cosim path) marks
input cones X-capable; `vcd_io::xprop_xmask_template_cosim` seeds every
primary input as X; and the GPU kernels clear the X-mask of each bit they
drive each edge — `state_prep` for the `build_edge_ops` driven set
(clock/reset/constants/model pins) and `gpu_apply_flash_din` for the SPI
MISO bits it writes directly (they bypass `state_prep`). The complement —
genuinely undriven inputs — stays X. `sim` keeps inputs known (driven from
the VCD) and pays no extra X-aware cost. End-to-end guards covering the
DFF and undriven-input X-sources, in both sim and cosim, live in
`tests/xprop_cosim/` (CI, fatal).

### Bidir tristate read-back mux (implemented 2026-06-04)

Point #2 above is now implemented (#96). `AIG::from_netlistdb`'s `bi_24t`
branch builds `Y = OE ? A : external` combinationally in the AIG —
`OR(AND(OE, A), AND(!OE, PAD))` via the De Morgan idiom already used by
`wire_dff_reset_set_overlay` — instead of the conservative `Y = PAD`. The
external arm is the same undriven PAD primary input (X under rule (1) until
a peripheral model drives it); the `OE=1` arm reads the core's own drive
`A`, so the loopback is **X-exact** (known whenever `A` is) and the
two-state read returns `A`, not the external stim. This removes bidir reads
from the "undriven input → X" subsumption for the `OE=1` case; they are now
exact rather than conservatively-X. `in_c`/`in_s` stay `Y = PAD`. Without
both `A` and `OE` pins the conservative `Y = PAD` still stands. Unit test:
`aig::gf180mcu_chip_top_tests::bi_24t_models_tristate_readback` evaluates
the full `Y` truth table. (#107's `$isunknown` x-assert work can now assert
bidir read-backs go definite when `OE` is asserted.)
