# Getting Started

This guide gets you from a fresh clone to **four working simulations**, in
increasing order of realism. Every command here runs against designs that ship
**in the repository** — no synthesis, no large downloads, no extra EDA tools.
Each one is also a CI job, so it is known-green.

> **Already have your own RTL?** Pass it directly to `jacquard sim design.v …`
> — the simulator auto-detects behavioral Verilog / SystemVerilog and
> synthesizes it transparently (see [Accepted RTL surface](accepted-rtl.md)).
> For peak GPU performance, or to use a commercial synthesizer (DC) for best
> mapping quality, see [Synthesis Flow](synthesis-flow.md). For UVM/cocotb/SVA
> questions see [Testbench Interop](interop.md).

## Prerequisites

A built `jacquard` binary. Either install it (see [Installation](installation.md))
or build from a clone:

```sh
git clone https://github.com/gpu-eda/Jacquard.git
cd Jacquard
git submodule update --init --recursive
cargo build -r --features metal --bin jacquard   # macOS / Apple Silicon
```

The examples below use `cargo run -r --features metal --bin jacquard -- …` so
they work straight from a clone. If you installed the binary (e.g.
`brew install gpu-eda/homebrew-tap/jacquard`), replace that prefix with just
`jacquard`. On Linux, swap `--features metal` for `--features cuda` (NVIDIA) or
`--features hip` (AMD), and replace the trailing `1` (NUM_BLOCKS) with 2× your
GPU's SM/CU count.

---

## Tier 1 — Your first simulation (a flip-flop)

The smallest possible run: a single D flip-flop. `jacquard sim` reads a
gate-level netlist plus a static input waveform (VCD) and writes an output VCD.

```sh
cargo run -r --features metal --bin jacquard -- sim \
    tests/timing_test/dff_test_synth.gv \
    tests/timing_test/dff_test.vcd \
    /tmp/dff_out.vcd \
    1
```

It finishes in well under a second (6 cycles). Confirm the captured `q` matches
the golden waveform:

```sh
diff <(grep -E '^[01]!' tests/timing_test/dff_test.vcd) \
     <(grep -E '^[01]!' /tmp/dff_out.vcd) && echo "MATCH"
```

That `MATCH` is your "Jacquard runs on this machine" checkpoint. The last
argument, `1`, is `NUM_BLOCKS` — always `1` for Metal; on CUDA/HIP set it to 2×
your GPU's SM/CU count.

---

## Tier 2 — A reactive testbench (`cosim`)

`jacquard sim` drives the design from a *fixed* input waveform. Real testbenches
need to *react* to the design's outputs. That is what `jacquard cosim` is for: it
runs peripheral models (UART, SPI flash, JTAG, bus monitors) as GPU kernels
alongside the design, so inputs can depend on outputs cycle-by-cycle.

The `dual_uart` design is two independent UART transmitters that send `"Hi"` and
`"OK"`. Cosim decodes the serial bits back into bytes — no input VCD required;
clock and reset come from the config.

```sh
cargo run -r --features metal --bin jacquard -- cosim \
    tests/dual_uart/dual_uart_synth.gv \
    --config tests/dual_uart/sim_config.json \
    --top-module dual_uart_top \
    --max-clock-edges 10000
```

Verify both channels decoded correctly:

```sh
python3 tests/dual_uart/check_pass.py target/test-out/dual_uart_events.json
# console: b'Hi' OK
# debug:   b'OK' OK
# PASS: both UART channels decoded correctly
```

The peripherals, pin mapping, baud rates, and clock are all declared in
[`tests/dual_uart/sim_config.json`](https://github.com/gpu-eda/Jacquard/blob/main/tests/dual_uart/sim_config.json).
See [Cosim execution model](adr/0017-cosim-execution-model.md) and
[Bus Transaction Tracing](bus-tracing.md) for the full peripheral set.

---

## Tier 3 — A real open-source PDK

Tiers 1 and 2 use Jacquard's abstract AIG cell library. Real designs come out of
synthesis mapped to a real **PDK** (process design kit). Jacquard decomposes
real standard cells — **SKY130** and **GF180MCU** today — into its AIG
representation directly, so a synthesized netlist simulates as-is.

### 3a. Pre-P&R (post-synthesis) — runs from the repo

A *pre-place-and-route* netlist is the output of logic synthesis, before layout
and before any SDF timing annotation. This is the fastest, most common
gate-level verification step. `logic_cone` is a small **SKY130** netlist (real
`sky130_fd_sc_hd__*` cells) that ships in the repo:

```sh
cargo run -r --features metal --bin jacquard -- sim \
    tests/timing_test/sky130_timing/logic_cone.v \
    tests/timing_test/sky130_timing/logic_cone.vcd \
    /tmp/logic_cone_out.vcd \
    1 --input-vcd-scope logic_cone
```

No `--sdf` flag: this is pure functional simulation of a real-PDK netlist. The
circuit is `4 DFFs → nand2/nor2/and2/inv cone → DFF`, computing
`Q = a & !b & !c & !d` (registered). Its sibling `inv_chain.v` (16 SKY130
inverters = identity) is a cleaner "output tracks input, delayed two cycles"
check if you want one.

> Add `--sdf logic_cone.sdf --sdf-corner typ` to layer Liberty cell delays on
> top — that moves you from functional to **timing-aware** simulation, covered
> in [Timing Simulation](timing-simulation.md).

### 3b. A full chip on GF180MCU

To see Jacquard handle a real, large design on a real PDK end-to-end, the
**wafer.space chess `chip_top`** (~227,000 cells, GlobalFoundries open-source
**GF180MCU** PDK) is wired up as a smoke-test recipe. The post-P&R netlist is
~200 MB so it is not committed — you regenerate it from the upstream
[LibreLane](https://github.com/Ravenslofty/gf180mcu-chess) flow. Full
step-by-step recipe, expected timings, and what it exercises:
[`tests/gf180mcu_chess_chip_top/README.md`](https://github.com/gpu-eda/Jacquard/blob/main/tests/gf180mcu_chess_chip_top/README.md).

This is the bridge to your own designs: synthesize to SKY130 or GF180MCU, then
run `jacquard sim` exactly as in 3a.

---

## Where to next

| You want to… | Go to |
|---|---|
| Simulate **your own behavioral RTL** (one command, auto-synthesized) | [Accepted RTL surface](accepted-rtl.md) |
| Prepare a **high-QoR gate-level netlist** (DC / Yosys, memory mapping) | [Synthesis Flow](synthesis-flow.md) |
| Run the large research **benchmarks** (NVDLA, Rocket, Gemmini) | [`benchmarks/README.md`](https://github.com/gpu-eda/Jacquard/blob/main/benchmarks/README.md) |
| Add **timing** (Liberty / SDF / violation checks) | [Timing Simulation](timing-simulation.md) |
| Use **UVM / cocotb / SVA**, or understand interop limits | [Testbench Interop](interop.md) |
| Add support for a **new PDK** | [Adding a New PDK](adding-a-pdk.md) |
| Understand **how it works** | [Simulation Architecture](simulation-architecture.md) |
