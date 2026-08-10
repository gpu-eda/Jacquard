# Testbench Interop (UVM, cocotb, SVA)

A common first question: *"Can I point my existing UVM / cocotb / SVA testbench
at Jacquard?"*

**Short answer: not today.** Jacquard is a gate-level *engine*, not a drop-in
replacement for a SystemVerilog or Python simulator's testbench runtime. This
page explains what works now, what is on the roadmap, and the fallback that
works for any flow today.

## What Jacquard drives today

| Mechanism | What it is | Reactive? |
|---|---|---|
| `jacquard sim` + input VCD | Replay a recorded input waveform through the netlist | No — inputs are fixed |
| `jacquard cosim` + peripheral models | UART, SPI flash, JTAG, Wishbone / APB3 monitors run as GPU kernels beside the design | Yes — inputs can depend on outputs cycle-by-cycle |

So *reactive* stimulus is supported — but through Jacquard's own
[peripheral model architecture](architecture/decisions/0013-plural-peripheral-configs.md) and
[cosim execution model](architecture/decisions/0017-cosim-execution-model.md), not through an
external testbench framework.

## The fallback that works for any flow: record-and-replay

You do **not** need framework integration to use Jacquard with a UVM, cocotb, or
plain-Verilog testbench. The universal path:

1. Run your existing testbench against any simulator that can dump a VCD
   (Verilator, Icarus, a commercial simulator, …).
2. Dump the design's **top-level input** pins to a VCD.
3. Replay that VCD through Jacquard with `jacquard sim`.

This is exactly what `jacquard sim` already is — a recorded-waveform replay — so
it works regardless of how the stimulus was generated. The trade-off is that the
replay is open-loop: it reproduces the *recorded* inputs, so it can't react to
divergent design behaviour. For closed-loop reactive stimulus, model the
peripheral as a cosim kernel instead.

## Roadmap

These are directions under consideration, not commitments or dated milestones:

- **SVA (SystemVerilog Assertions)** — *planned.* Jacquard already lowers a class
  of immediate assertions through synthesis (`GEM_ASSERT` cells; see the
  assertion handling in `aigpdk.rs`). Broader SVA support is the next step here.
- **Running UVM test suites** — *design settled, unbuilt.* The shape is
  [Decision 0022](architecture/decisions/0022-flow-controlled-io.md): split the testbench the way
  emulators have since SCE-MI. Sequences, randomisation and checking stay on the
  host; the driver's *timed* half is rewritten as a synthesizable transactor and
  compiled into the AIG beside the design (possible since the
  [RTL on-ramp](architecture/decisions/0021-behavioral-rtl-support.md) shipped), so it wiggles pins
  at GPU speed. Existing UVM drivers don't port as-is, and that isn't a gap we
  can close: they drive every cycle, and cosim runs 1024 edges per dispatch with
  the CPU absent, which is where the speed comes from.
- **cocotb** — *needs more work.* A naive bridge would marshal Python ↔ GPU every
  cycle, which would dominate runtime and erase the GPU speedup. Same cliff as
  above, same fix: exchange transactions, not cycles. Making cocotb *performant*
  against Jacquard needs more design thinking, not just a shim.

If any of these is blocking for you, the record-and-replay path above is the
recommended interim approach.

## See also

- [Getting Started](getting-started.md) — `sim` and `cosim` walkthroughs
- [Bus Transaction Tracing](bus-tracing.md) — protocol-aware observation of on-chip buses
- [Cosim execution model](architecture/decisions/0017-cosim-execution-model.md) — how peripheral kernels run
