# Plan: selective X-propagation in cosim

Extends [ADR 0016](../adr/0016-selective-x-propagation.md) (selective
X-propagation, today `sim`-only) to the reactive `cosim` path. Issue:
[#95](https://github.com/gpu-eda/Jacquard/issues/95).

## Where it stands

X-prop is wired into `sim` only. `cosim` always runs **two-state**, so
uninitialised DFF/SRAM and undriven inputs silently resolve to `0` —
producing false agreement against 4-state RTL and masking init-order
bugs (the exact failure `--xprop` exists to surface).

What already exists and is reusable:

- The **Metal `simulate_v1_stage` kernel is already X-capable**
  (`sram_xmask` buffer, `xmask_state_offset`, the X-mask read logic in
  `kernel_v1.metal`). cosim dispatches the same kernel — no GPU core
  change.
- `DesignArgs.xprop` already threads into `script.xprop_enabled` (via
  `setup`); `cmd_cosim` just hardcodes `xprop: false`.
- Host machinery on the sim side: `expand_states_for_xprop`, the
  `sram_xmask` shadow (init `0xFFFF_FFFF`), `split_xprop_states`,
  `write_output_vcd_xprop`.

So this is **host-side reactive plumbing**, not new kernel work.

## X-source taxonomy (the semantic to get right)

X originates from four places; cosim must model all four:

| Source | X when… | Mechanism |
|--------|---------|-----------|
| Uninitialised **DFF** | power-up, before first clocked write | `expand_states_for_xprop` seeds the X-mask half (as in `sim`) |
| Uninitialised **SRAM** | before first write to a cell | `sram_xmask` shadow init `0xFFFF_FFFF`, carried across ticks |
| **Undriven input pad** | no model / constant / clock / reset drives it | input X-mask = X for every primary-input bit *not* in the driven set |
| **Bidir pad input side** | `OE` deasserted and nothing external drives it | per-edge: input X-mask = `OE ? known : X`, reading `OE` (the `__oe` observable) |

The first two are sequential power-up X (ADR 0016's original scope). The
**last two are new** — input/IO X-sources the `sim` static-VCD path never
had to model, and the reason this is more than "carry the sim machinery
over."

**Driven set** (bits that are *known*, X-mask cleared): scheduler-driven
clock(s) + reset, each peripheral model's `driven_positions()`, and
`constant_inputs` / `constant_ports`. The complement of the driven set
within the primary inputs is X.

## Phases

### Phase 1 — Flag plumbing
- Add `xprop: bool` to `CosimArgs`; stop hardcoding `xprop: false` in
  `cmd_cosim`'s `DesignArgs`. (`setup` already turns it into
  `script.xprop_enabled`.)

### Phase 2 — X-state in the cosim loop
- Expand the cosim state buffer with `expand_states_for_xprop` and
  allocate the `sram_xmask` shadow (init `0xFFFF_FFFF`), carried across
  the per-tick loop.
- Pass both into the simulate dispatch (`encode_dispatch` /
  `simulate_v1_stage` already accept `sram_xmask` + the xmask offset).
- Set the kernel's `xmask_state_offset` metadata for the cosim layout.

### Phase 3 — Input X-mask policy + per-edge maintenance (the novel part)
- **Init:** every primary-input X-mask bit = X, except the driven set
  (clock/reset/model/constants), which start known.
- **Per edge:** wherever `state_prep` / model `ModelOverrides` drive an
  input bit, also clear that bit's X-mask (driven ⇒ known). Undriven
  bits stay X.
- **Bidir:** after each tick, read each `bi_24t` pad's `OE` (the `__oe`
  observable already surfaced by the AIG postprocess) and set the pad's
  input-side X-mask = `OE ? known : X` for the next edge. This OE→input
  feedback is the trickiest sub-item; isolate it behind a small
  per-edge "bidir X refresh" step.

### Phase 4 — Observe-kernel offset fix (intersects recent work)
- `gpu_io_step`'s output reads (`READ_OUT_BIT` → `states[state_size +
  …]`) for UART, Wishbone, **and the new bus-trace** assume the
  two-state layout. Under xprop the slot doubles `[value | xmask]`, so
  the output-**value** offset shifts. Thread the correct output-value
  offset (from the script's xprop metadata) into `gpu_io_step` and the
  Rust drain/`READ_OUT_BIT` so the observe kernels read the value half,
  not the X-mask half. Add a guard/test so this can't silently regress.

### Phase 5 — X-aware VCD output
- cosim emit path uses a `write_output_vcd_xprop`-equivalent so traced
  nets and top-level IO emit `x` (not `0`) where unknown; the bidir
  `__out`/`__oe` split already exists and should reflect X correctly.

### Phase 6 — Verification
- A small reactive test design with (a) an unreset register, (b) an
  unconnected input pad, (c) a bidir pad toggling OE — assert the output
  VCD shows `x` until each is resolved (clocked write / model drive / OE
  assert), and `0`/`1` after. Pairs with the JTAG-replay path.
- Where feasible, extend the CPU `sanity_check_cpu_xprop` parity to a
  cosim scenario.

## Risks / open questions

- **Bidir OE feedback** (Phase 3): OE is combinational within a tick;
  the input-side X for edge *N+1* depends on OE computed at edge *N*.
  Confirm this one-edge latency is acceptable (it matches how a real
  pad's direction settles) or whether intra-tick resolution is needed.
- **Observe-offset regression** (Phase 4): the highest-risk interaction;
  the bus-trace code is days old. Needs an explicit test under `--xprop`.
- **Performance:** the state buffer doubles again on top of any timing
  expansion; the VCD ring-buffer snapshot grows. Measure on a real
  JTAG-replay run.
- **SRAM xmask carry:** confirm the shadow persists correctly across the
  batched/single-tick dispatch modes the cosim loop uses.

## ADR / docs impact

- **Amend ADR 0016**: record the cosim extension and broaden the
  X-source taxonomy to include undriven input pads + bidir-OE (the
  original ADR covered only sequential power-up X).
- Fold the IO X-source rules into `docs/selective-x-propagation.md`.
- Update the cosim `--xprop` help + `docs/installation.md` once shipped.
