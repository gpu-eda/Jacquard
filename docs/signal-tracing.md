# In-Design Signal Tracing (`--trace-signals`)

## Overview

By default a Jacquard output VCD contains only top-level IO.
`--trace-signals <FILE>` surfaces **user-selected internal nets** in that
VCD alongside the top-level ports — so you can watch a DFF's Q, a
controller state bit, or an SRAM port wire without re-synthesizing or
exposing it as a port.

It is available on both `jacquard sim` and `jacquard cosim`, and is
observe-only: traced nets are read out each tick, never driven.

Each name in the file is resolved against the netlist, registered as a
primary output **before partitioning** (so it gets a state-buffer slot),
and emitted on the same path as the top-level IO. It works uniformly for
sequential (DFF Q) and combinational nets — anything that has a name in
the netlist database.

This is the **raw-wire** counterpart to
[bus transaction tracing](bus-tracing.md): `--trace-signals` gives you
per-cycle waveforms of individual nets; bus tracing gives you decoded
transaction records. Use this when you want a waveform; use bus tracing
when you want `READ 0x40 => 0x1`.

## File format

One hierarchical signal name per line:

```text
# JTAG debug-module state (comments and blank lines are ignored)
chip_core.dm.haltreq_q[0]
chip_core.dm.haltreq_q[1]

# Yosys-internal nets — same syntax works
chip_core.sram_u._00147_

# A whole bus, one bit per line
data0_obs[0]
data0_obs[1]
```

- Blank lines and lines whose first non-whitespace character is `#` are
  skipped.
- Hierarchy uses `.` as the separator; a trailing `[N]` selects a bus
  bit.
- A leading backslash (Verilog escaped-identifier syntax) is stripped.

A real example ships in `tests/jtag_minimal/trace_signals.txt`.

## Name resolution

Post-synthesis net names are ambiguous — Yosys may flatten a hierarchy
into one escaped identifier (`\soc.sram.read_port__data`), expand a bus
into per-bit scalars (`soc.bus__addr[3]`), or preserve real structural
hierarchy. Rather than guess, the resolver tries multiple candidate
interpretations of each name and takes the first that matches the netlist
database, so the **same syntax works across all three conventions**.

- **Unresolved names warn, they don't abort.** A bad name logs a warning
  and is skipped; the rest of the list still registers. A trailing
  summary line reports how many signals registered vs. were dropped, so a
  mistyped list surfaces clearly at startup:

  ```
  --trace-signals: registered 34 signal(s), dropped 2 (file: trace.txt)
  ```

- Names that resolve to a **constant** (tied 0/1) are skipped — there's
  nothing to observe at runtime.

## Where the output lands

Traced nets appear in whichever VCD the run already emits:

| Command | Flag | Traced nets appear in |
|---------|------|------------------------|
| `jacquard sim`   | `--trace-signals <FILE>` | the output VCD |
| `jacquard cosim` | `--trace-signals <FILE>` | the `--output-vcd` output **only** |

They show up as ordinary VCD wires next to the top-level IO, named by the
string you put in the trace file.

> **cosim:** traced nets land in `--output-vcd` **only**. The
> `--stimulus-vcd` carries primary inputs and does *not* include them, so
> if you trace a net and look in the stimulus VCD you'll see nothing.
> `--output-vcd` does **not** require timing data — see
> [Pre-PnR functional runs](#pre-pnr-functional-runs).

### Pre-PnR functional runs

`--output-vcd` is the functional output path too — it does **not** require
`--timing-ir`/SDF. Run a synthesized (pre-PnR) netlist through `cosim`
with `--output-vcd out.vcd` and you get chip outputs and traced nets per
cycle, with transitions at clock edges (no arrival-time offsets). This is
the right mode for functional / 4-state X-pessimism debugging, where
there is no timing data to supply yet. Adding `--timing-ir` later only
adds arrival-time offsets to the same VCD.

### Top-level inout (bidir) pads

A top-level `inout` pad is split into two observables in the output VCD:
`<pad>__out` (the value the core drives) and `<pad>__oe` (the pad's
output-enable). The raw `<pad>` net reads the pad's **input** side, so on
an output-only or undriven cycle it can look flat — watch `<pad>__out` /
`<pad>__oe` to see what the design is driving. Example:
`bidir_PAD[12]__out`, `bidir_PAD[12]__oe`. These appear automatically;
you don't need to list them in the trace file.

## Finding signal names

Use the `netlist-graph` tool (see the project README) to discover the
exact post-synthesis names:

```bash
# Search for nets matching a pattern
uv run netlist-graph search <netlist.v> "haltreq"

# Trace what drives / loads a signal (to find nearby observable nets)
uv run netlist-graph drivers <netlist.v> "soc.cpu.state" -d 5
uv run netlist-graph loads   <netlist.v> "soc.cpu.ack"   -d 5

# Emit a ready-to-use trace file
uv run netlist-graph watchlist <netlist.v> out.json signal1 signal2 ...
```

## SRAM observability workflow

The recommended way to observe SRAM port activity is wire-level tracing
rather than the env-var-gated `JACQUARD_SRAM_DUMP`. `netlist-graph` can
discover the port wires and emit a trace file directly:

```bash
# 1. Discover SRAM port wire names from the netlist
uv run netlist-graph sram-ports design.v --cell-type SRAM -o sram_trace.txt

# 2. Surface them in the VCD with full per-tick accuracy
jacquard cosim design.v --config sim.json \
    --trace-signals sram_trace.txt --output-vcd out.vcd

# 3. Post-process the VCD to reconstruct bus values
```

## Example

`tests/jtag_minimal/` uses `--trace-signals` to surface the debug
module's observable outputs (`dmactive_obs`, `haltreq_obs`,
`data0_obs[0..31]`) so the test's pass criterion can check that the magic
value `0xCAFEBABE` lands in `data0_obs`:

```bash
jacquard cosim tests/jtag_minimal/data/top.pnl.v \
    --config tests/jtag_minimal/sim_config.json \
    --trace-signals tests/jtag_minimal/trace_signals.txt \
    --jtag-replay tests/jtag_minimal/data/bitbang.rec \
    --output-vcd out.vcd
```

## Troubleshooting

| Symptom | Cause / fix |
|---------|-------------|
| `not found in netlistdb (tried N candidate(s))` | The name doesn't exist post-synthesis under any candidate spelling. Find the real name with `netlist-graph search`; the net may have been renamed or optimized away. |
| Signal registered but flat in the VCD | It may resolve to a constant after optimization (the startup log notes constants are skipped), or the cone was stripped. Confirm it's a live net with `netlist-graph drivers`. |
| Nothing appears in the VCD | For cosim, traced nets only land in `--output-vcd` (not `--stimulus-vcd`) — make sure you passed it. Also check the startup summary reports a non-zero registered count. |

## Implementation notes

Registration happens at AIG construction, before partitioning, which is
why the list must be supplied via the CLI flag (not a runtime env var).
The mechanism lives in `src/sim/trace_signals.rs`; emission piggybacks on
`emit_extra_observables` in `src/sim/vcd_io.rs`. The same multi-candidate
resolver backs bus-trace pin binding (see
[bus tracing](bus-tracing.md) and ADR 0013).
