# Bus Transaction Tracing (AHB / APB)

## Overview

`jacquard cosim` can decode on-chip bus transactions and emit them in a
compact, transaction-level form — one row per transfer, rather than raw
per-cycle waveforms. You declare the bus interfaces to watch in
`sim_config.json`; cosim observes their pins on the GPU each tick and
runs the protocol decode on the CPU, writing decoded transactions to a
CSV file.

This is **observe-only**: the tracer watches signals the design already
drives, it never drives anything. It adds no measurable simulation
overhead when no buses are configured.

| Protocol | Status |
|----------|--------|
| **APB3** | Supported |
| AHB-Lite | Planned (pipelined address/data pairing, burst tracking) |
| AHB5     | Planned (AHB-Lite + security / exclusive signals) |

The design rationale lives in
[ADR 0013](adr/0013-plural-peripheral-configs.md); the roadmap is in
[`plans/bus-transaction-tracing.md`](plans/bus-transaction-tracing.md).

Bus tracing is the structured, protocol-aware counterpart to
[`--trace-signals`](signal-tracing.md), which surfaces *raw* internal
nets in the output VCD. Use `--trace-signals` when you want waveforms of
individual wires; use bus tracing when you want decoded
`READ 0x40 => 0x1` records.

## Configuring a bus

Add a `bus_traces` array to `sim_config.json`. Each entry names one bus
interface:

```json
{
    "netlist_path": "build/soc.gv",
    "clock_gpio": 0,
    "reset_gpio": 1,
    "num_cycles": 100000,
    "clock_period_ps": 40000,

    "bus_traces": [
        {
            "name": "dmi",
            "protocol": "apb3",
            "prefix": "soc.dm.",
            "addr_bits": 9,
            "data_bits": 32
        }
    ]
}
```

| Field | Required | Meaning |
|-------|----------|---------|
| `name` | yes | Label for this bus in the CSV `bus` column. |
| `protocol` | yes | `apb3` (or `ahb-lite` / `ahb5` once supported). |
| `prefix` | yes | Hierarchical net-name prefix; standard pin names are appended (see below). May be `""` for top-level pins. |
| `addr_bits` | no (default 32) | Address bus width. |
| `data_bits` | no (default 32) | Data bus width. |
| `signals` | no | Per-pin net-name overrides (see [Pin resolution](#pin-resolution)). |

### Pin names

By default each protocol pin is resolved as `{prefix}{pin}`. For APB3:

| Logical pin | Default net | Notes |
|-------------|-------------|-------|
| `psel`    | `{prefix}psel`    | required |
| `penable` | `{prefix}penable` | required |
| `pwrite`  | `{prefix}pwrite`  | direction |
| `paddr`   | `{prefix}paddr[i]` | `addr_bits` wide |
| `pwdata`  | `{prefix}pwdata[i]` | `data_bits` wide |
| `prdata`  | `{prefix}prdata[i]` | `data_bits` wide |
| `pready`  | `{prefix}pready`  | optional — **unresolved is treated as always-ready (1)** |
| `pslverr` | `{prefix}pslverr` | optional — unresolved is treated as no-error (0) |

So a bus with `"prefix": "soc.dm."` looks for `soc.dm.psel`,
`soc.dm.paddr[0]`, …, `soc.dm.prdata[31]`.

If your design's pins don't follow that convention, remap individual
logical pins with `signals`:

```json
{
    "name": "periph",
    "protocol": "apb3",
    "prefix": "soc.apb.",
    "signals": {
        "psel": "soc.apb_decode.sel_periph",
        "prdata": "soc.apb_mux.readback"
    }
}
```

## Running

```bash
cargo run -r --features metal --bin jacquard -- cosim \
    build/soc.gv \
    --config sim_config.json \
    --bus-trace-csv bus.csv
```

At startup each bus logs whether it resolved:

```
bus-trace `dmi` (APB3): psel/penable resolved, addr 9/9 bits, pready=true pslverr=true
```

and at the end:

```
bus-trace: decoded 12 transaction(s) across 1 bus(es)
bus-trace: wrote 12 transaction(s) to bus.csv
```

## CSV output

```
tick,bus,protocol,dir,addr,data,resp,burst
24,dmi,apb3,WR,0x10,0xCAFEBABE,OK,
30,dmi,apb3,RD,0x10,0xCAFEBABE,OK,
```

| Column | Meaning |
|--------|---------|
| `tick` | Cosim edge at which the transfer completed. One clock cycle = 2 edges (rising + falling) for a single-domain design. |
| `bus` | The configured bus `name`. |
| `protocol` | `apb3` / `ahb-lite` / `ahb5`. |
| `dir` | `WR` or `RD`. |
| `addr` | Transfer address (hex). |
| `data` | `pwdata` for writes, `prdata` for reads (hex). |
| `resp` | `OK` or `ERR` (from `pslverr` / `hresp`). |
| `burst` | AHB burst position `beat/len` (empty for APB). |

## Pin resolution

For the GPU to read a bus pin each tick, that net must (1) exist in the
post-synthesis netlist under a resolvable name and (2) survive into the
simulation's output state. Two consequences:

- **Names must survive synthesis.** The resolver uses the same
  multi-candidate matcher as `--trace-signals`, so Yosys-flattened
  (`\soc.dm.psel`), scalar-expanded (`soc.dm.paddr[3]`), and
  structurally-hierarchical names all work. But synthesis is free to
  rename or delete *combinational* nets. The robust pattern is to make
  the bus signals **registers** (their DFF Q outputs keep their names),
  or to annotate the RTL nets with `(* keep *)`.

- **Constant-folded bits read as 0 — correctly.** If a design only ever
  drives, say, addresses `0x00` and `0x04`, synthesis folds every
  address bit except `paddr[2]` to a constant. The startup log then
  shows e.g. `addr 1/8 bits`. This is expected: the tracer reconstructs
  the full value correctly because the dropped bits are genuinely 0.

`pready` / `pslverr` are allowed to be absent. A common case is an
always-ready slave that ties `pready` high — it folds to a constant,
fails to resolve, and the tracer correctly treats the bus as
always-ready.

## Worked example

`tests/apb_trace/` is a self-contained, synthesizable APB3 system used
as the CI regression. Its master issues a fixed program — two writes
then two reads — to a register-file slave, and `check.py` asserts the
decoded CSV. See [`tests/apb_trace/README.md`](../tests/apb_trace/README.md).

```bash
yosys -s tests/apb_trace/synth.tcl          # (from tests/apb_trace/)
cargo run -r --features metal --bin jacquard -- cosim \
    tests/apb_trace/apb_trace_synth.gv \
    --config tests/apb_trace/sim_config.json \
    --top-module apb_trace \
    --max-clock-edges 200 \
    --bus-trace-csv apb.csv
python3 tests/apb_trace/check.py apb.csv
```

## Troubleshooting

| Symptom | Cause / fix |
|---------|-------------|
| `psel/penable did not resolve … this bus will not capture` | The `prefix` is wrong, or the nets were optimized away. Find the real names with `uv run netlist-graph search <netlist> psel`, then fix `prefix` or add `signals` overrides. |
| Zero transactions decoded | Gate never asserted. Check that `psel`/`penable` resolve (startup log) and that the bus is actually exercised within `--max-clock-edges`. |
| Address or data always 0 | `paddr`/`pwdata`/`prdata` nets didn't resolve (renamed/folded). Confirm with `netlist-graph search`; mark the RTL nets `(* keep *)` and re-synthesize. |
| Reads return stale/wrong data | The slave must present `prdata` during the ACCESS phase. Register `prdata` so its value is stable when `psel & penable` are high. |

## Limitations

- APB3 only for now; AHB-Lite / AHB5 and annotated-VCD output are the
  next phases (see the [plan](plans/bus-transaction-tracing.md)).
- Up to 4 buses per run, addresses/data up to 32 bits.
- Cosim is Metal-only today, so bus tracing is Metal-only.
- The legacy hardcoded Wishbone trace (a separate, SoC-specific path) is
  unaffected; folding it onto this general mechanism is a planned
  follow-up.
