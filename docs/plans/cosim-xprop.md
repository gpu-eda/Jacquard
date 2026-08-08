# Plan: selective X-propagation in cosim

Extends [Decision 0016](../architecture/decisions/0016-selective-x-propagation.md) (selective
X-propagation, today `sim`-only) to the reactive `cosim` path. Issue:
[#95](https://github.com/gpu-eda/Jacquard/issues/95).

## Where it stands

> **Update 2026-06-03:** while extending to cosim we found the seed
> template was broken for **both** paths — `expand_states_for_xprop`
> cleared the X-mask for all `input_map` positions, which includes DFF-Q
> feedback reads, so uninitialised DFFs read as known `0` and X never
> surfaced even in `sim`. Fixed via `vcd_io::xprop_xmask_template`
> (X at genuine X-sources only) plus output-slot seeding in `run_cosim`.
> See the handoff and decision-0016 amendment. Phases 1, 2, 5, the seed fix,
> **phase 3** (undriven inputs → X: `compute_x_capable_pins(treat_inputs_as_x)`
> gated by `DesignArgs::xprop_undriven_inputs`; `xprop_xmask_template_cosim`
> seeds inputs X; `state_prep` + `gpu_apply_flash_din` clear the X-mask of
> each bit they drive), **phase 6** (end-to-end `tests/xprop_cosim/`
> guards in CI, sim + cosim) and **phase 4** (observe-kernel output-offset:
> correct by construction — `gpu_io_step` reads the value half via
> `uart_params.state_size = effective_state_size`; guarded by re-running the
> APB3 + dual-UART cosims under `--xprop` and asserting identical decoded
> output) are done. **All phases of #95 are complete**; the bidir
> tristate-mux read `Y = OE ? A : external` is separately tracked as #96.

`--xprop` is now wired into **both** `sim` and `cosim`. (Historically it
was `sim`-only and `cosim` always ran two-state, silently resolving
uninitialised DFF/SRAM and undriven inputs to `0` — false agreement
against 4-state RTL; that gap is what this plan closed.)

What the work built on (already existed and was reused):

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

The first two are sequential power-up X (Decision 0016's original scope). The
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

### Phase 4 — Observe-kernel offset (DONE — correct by construction)
- `gpu_io_step`'s output reads (`READ_OUT_BIT` → `states[state_size + …]`)
  for UART, Wishbone, and bus-trace turned out to be **already correct**
  under xprop: `state_size` here is `uart_params.state_size`, set to the
  full `effective_state_size`, so `states[state_size + word]` indexes the
  output slot and `word < reg_io_state_size` lands in the value half (value
  is at the front of `[value | xmask | …]`). The earlier worry that "the
  offset shifts" was unfounded — no Rust/kernel change was needed.
- **Guard added** so it can't silently regress: CI re-runs the APB3
  bus-trace and dual-UART cosims under `--xprop` and asserts the decoded
  transactions/bytes are identical to the two-state runs (both designs are
  fully reset-driven, so phase-3 undriven-input X never reaches the traced
  signals). If a future layout change made the observe reads hit the xmask
  half, the decoded values would corrupt and the checkers would fail.

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

## decision / docs impact

- **Amend Decision 0016**: record the cosim extension and broaden the
  X-source taxonomy to include undriven input pads + bidir-OE (the
  original decision covered only sequential power-up X).
- Fold the IO X-source rules into `docs/selective-x-propagation.md`.
- Update the cosim `--xprop` help + `docs/installation.md` once shipped.
