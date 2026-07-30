# Debugging X values in cosim (`xsources` + `xroots`)

When you run `jacquard cosim --xprop` and a signal reads `x` (unknown), the
question is always *why* — which uninitialised state or undriven input is
feeding that X forward? This guide covers the two-command workflow that
answers it as a **static query**, without the trace→guess→re-run loop.

- `jacquard xsources` — enumerate *where X originates* in the design.
- `netlist-graph xroots` — given a signal, find *which* of those X-sources
  reach it (its backward "X-root" frontier).

See [Decision 0016](architecture/decisions/0016-selective-x-propagation.md) for the X-propagation
semantics and [issue #98](https://github.com/gpu-eda/Jacquard/issues/98) for
the design rationale.

## Background: where X comes from

Under `--xprop`, an X value originates at one of three **X-sources** and then
propagates forward through combinational logic:

| Kind | X because… |
|------|-----------|
| `unreset-dff` | A DFF Q output with **no reset/set pin** — undefined at power-up and never forced to a known value. |
| `reset-dff` | A DFF Q with a reset/set pin — undefined at power-up but **defined once reset asserts**. Usually *not* the culprit after reset. |
| `sram-read` | An SRAM read-data port — undefined until the addressed cell is written. |
| `undriven-input` | A primary input the testbench **leaves undriven** (no clock/reset/constant/peripheral drives it) — reads X every tick. |

A signal reads X iff at least one X-source lies in its backward logic cone.
That is a pure netlist property — no simulation needed to compute it.

## Step 1 — enumerate X-sources: `jacquard xsources`

```bash
jacquard xsources <netlist> --config <sim_config.json> -o xsources.json
```

This builds the static AIG (no GPU) and writes a JSON manifest of every
X-source. `--config` is required only to classify `undriven-input` sources
(the driven-set complement); without it, only DFF and SRAM sources are
emitted.

```json
{
  "schema_version": "1.0",
  "netlist": "design.v",
  "undriven_inputs_classified": true,
  "x_sources": [
    { "net": "q_unreset",  "kind": "unreset-dff",    "cell": "_29_" },
    { "net": "_10_[1]",    "kind": "unreset-dff",    "cell": "_30_" },
    { "net": "q_reset",    "kind": "reset-dff",      "cell": "_28_" },
    { "net": "unconnected","kind": "undriven-input" }
  ]
}
```

Notes:

- **`net`** is jacquard's canonical net name (after assign-merging), which may
  differ from the raw name in the Verilog. **`cell`** is the driving instance
  and is stable across tools — `xroots` resolves DFF/SRAM sources by cell, so
  the naming difference does not matter.
- The `undriven-input` classification reflects the config's **declared
  drivers**: clock(s), reset, `constant_inputs`, `constant_ports`, and each
  peripheral's pins (flash/uart/gpio/jtag). An input outside that set is
  reported as undriven.
- A reset-connected DFF is emitted as `reset-dff`. It is X at power-up but
  resolves once reset asserts, so `xroots` traverses **through** it rather
  than stopping (see below).

## Step 2 — find a signal's X-roots: `netlist-graph xroots`

```bash
netlist-graph xroots <netlist> <signal> --xsources xsources.json
```

`xroots` walks **backward** from `<signal>` through the driver cone — and
*through* DFF data (`D`) pins, the reverse of the forward X-prop fixpoint —
skipping clock/set/reset pins. It stops at each **persistent** X-source
(`unreset-dff` / `sram-read` / `undriven-input`), reporting the nearest
frontier:

```text
$ netlist-graph xroots design.v top.cpu.stall --xsources xsources.json
X-source frontier of top.cpu.stall (2 root(s), nearest first):
  [unreset-dff] top.cpu.state[3]   (depth 5)
  [undriven-input] cfg_mode        (depth 8)
```

Reset DFFs (`reset-dff`) are **traversed through** (their data cone is
followed), because reset defines them — so the frontier surfaces the *real*
persistent roots behind them, not the reset flop itself.

If no X-source reaches the signal, `xroots` says so — under `--xprop` that
signal should be X-free.

### Without a manifest

`xroots` also runs without `--xsources`, classifying X-sources from the
netlist alone (DFF Q by reset-pin presence, SRAM reads, and genuinely
undriven internal nets). This cannot classify `undriven-input` sources (it
does not know the cosim driven set), so pass `jacquard xsources --config`
output when you need those.

## Step 3 — confirm with a targeted `--xprop` run

`--emit-trace` writes the frontier as a [`--trace-signals`](signal-tracing.md)
file, so confirming the X actually originates where `xroots` says is one
command:

```bash
netlist-graph xroots design.v top.cpu.stall \
    --xsources xsources.json --emit-trace xroots.txt

jacquard cosim design.v --config sim.json \
    --xprop --trace-signals xroots.txt --output-vcd out.vcd
```

The traced frontier nets appear in `out.vcd`; watch them carry `x` and resolve
(or not) over the reset window to confirm the diagnosis.

## Worked example

`tests/xprop_cosim/` is the X-propagation demo. `q_unreset` is a self-holding
unreset register (stays X); `q_reset` resolves after reset:

```bash
jacquard xsources tests/xprop_cosim/xprop_demo_synth.gv \
    --config tests/xprop_cosim/sim_config.json -o /tmp/xsrc.json

# Why is the counter's next-state X?
netlist-graph xroots tests/xprop_cosim/xprop_demo_synth.gv "_11_[3]" \
    --xsources /tmp/xsrc.json
#   [unreset-dff] unreset_count[3]  (depth 2)
#   [unreset-dff] unreset_count[2]  (depth 3)
#   ...
```

## Limitations

- The frontier is a **static over-approximation**: it reports X-sources whose
  cone reaches the signal, not whether the X is live on a given cycle (an
  unreset DFF may resolve once its own data cone settles from known inputs).
  It tells you *what to initialise/drive* to make the signal defined.
- Name round-tripping is exact for **flattened** post-synthesis netlists (the
  target of these tools). DFF/SRAM sources resolve by cell instance, which is
  robust; `undriven-input` sources resolve by name.
- Dominator analysis (X-sources that *every* path passes through — guaranteed
  roots) is a planned follow-up; v1 reports the reachable frontier.
