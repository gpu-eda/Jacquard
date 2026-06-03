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

### Phase 2 — X-state in the cosim loop  ✅ done (1ba01eb)
- Expanded the cosim state buffer via `expand_states_for_xprop` and
  allocated the `sram_xmask` shadow (init `0xFFFF_FFFF`), bound at
  `simulate_v1_stage` buffer(7) through the dispatch chain.
- No `write_params` change needed: `is_x_capable` / `xmask_state_offset`
  are baked into the *script* at flatten time and `state_size` already
  uses `effective_state_size` (so the layout scales automatically).

### Phase 3 — Input X-mask policy + per-edge maintenance (the novel part)
- **Init:** every primary-input X-mask bit = X, except the driven set
  (clock/reset/model/constants), which start known. The
  `expand_states_for_xprop` template clears *all* inputs to known, so
  Phase 3 re-marks the *undriven* inputs back to X.
- **Per edge:** wherever `state_prep` / model `ModelOverrides` drive an
  input bit, also clear that bit's X-mask (driven ⇒ known). Undriven
  bits stay X.
- **Bidir: no special handling in this issue.** A `bi_24t` pad's
  core-read is modelled `Y = PAD` (tristate not modelled — `aig.rs`),
  and the PAD net is an undriven primary input, so bidir reads fall out
  of the generic undriven-input rule above as **X** — conservative and
  safe (an unmodelled bidir read surfaces as unknown, never a false
  0/1). The earlier "per-edge OE→input feedback with one-edge latency"
  idea was **wrong**: the correct read is combinational
  `Y = OE ? A : external`, which requires modelling the tristate mux in
  the AIG. That is deferred to
  [#96](https://github.com/gpu-eda/Jacquard/issues/96); until it lands,
  an `OE=1` loopback reads pessimistic-X (safe).

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
- A small reactive test design with (a) an unreset register and (b) an
  unconnected input pad — assert the output VCD shows `x` until each is
  resolved (clocked write / model drive), `0`/`1` after. (Bidir
  read-back correctness is deferred to #96.)
- Where feasible, extend the CPU `sanity_check_cpu_xprop` parity to a
  cosim scenario.

## Risks / open questions

- **Bidir read-back is pessimistic-X** until [#96](https://github.com/gpu-eda/Jacquard/issues/96)
  models the tristate mux (`Y = OE ? A : external`). Safe (false-X, not
  false-0); resolve by driving the pad's external side with a model.
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
