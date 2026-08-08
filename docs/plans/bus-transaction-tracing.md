# Plan: Config-driven AHB/APB bus transaction tracing

## Goal

Trace **AHB5, AHB-Lite, and APB3** bus transactions in cosim, compactly,
without baking signal names into source. Output as **CSV** (machine-readable
transaction table) and **annotated VCD** (transactions as a signal group for
waveform viewers). Decode site: **GPU capture + CPU protocol FSM** (the kernel
stays dumb; protocol semantics live in testable Rust).

Order: **APB3 first** (validate against the Hazard3 JTAG-DM APB DMI in
`tests/jtag_minimal/`), then AHB-Lite, then AHB5.

## Why this shape

The existing "Wishbone bus trace" (`build_wb_trace_params`,
`cosim_metal.rs:1277`; `gpu_io_step`, `kernel_v1.metal:1182`) proves the
*mechanism* — a GPU observe-only peripheral that packs a compact per-tick
entry into a ring buffer only when the bus is active/changed, drained by the
CPU — but it is **hardcoded** to one VexRiscv-style SoC (literal names
`cpu.fetch.ibus__cyc`, `spiflash.ctrl.wb_bus__ack`, …). We generalize that
mechanism into a config-driven, protocol-aware monitor. It is observe-only
(we watch design outputs, never drive), so it fits the decision-0013 GPU
observe-only peripheral pattern, and gets the `effective_*()`-style plural
config for free.

Two existing pieces are reused:
- **Multi-candidate name resolution** in `src/sim/trace_signals.rs` — handles
  Yosys-flattened / scalar-expanded / structural hierarchical naming. Refactor
  the candidate generator into a shared helper so the bus tracer binds pins the
  same way `--trace-signals` does.
- **Extra-observables VCD path** (`emit_extra_observables`, `vcd_io.rs:635`) —
  the model for emitting synthesized signals into the output VCD.

The hardcoded WbTrace is **left intact** for now (it has a passing test);
migrating it onto the general mechanism is a clean follow-up, not a
prerequisite.

## Design

### 1. Config schema — `src/testbench.rs`

```rust
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BusProtocol { Apb3, AhbLite, Ahb5 }

#[derive(Debug, Clone, Deserialize)]
pub struct BusTraceConfig {
    pub name: String,
    pub protocol: BusProtocol,
    /// Hierarchical prefix; standard protocol pin names are appended.
    pub prefix: String,
    #[serde(default = "default_addr_bits")] pub addr_bits: usize, // 32
    #[serde(default = "default_data_bits")] pub data_bits: usize, // 32
    /// Optional per-pin overrides: logical pin name -> explicit net name,
    /// for designs whose pins don't follow `{prefix}{PIN}`.
    #[serde(default)] pub signals: HashMap<String, String>,
}
```

Add to `TestbenchConfig`:
```rust
#[serde(default)] pub bus_traces: Vec<BusTraceConfig>,
```
New feature, so no singular legacy form. (`effective_bus_traces()` provided
for symmetry with `effective_uarts()`, even though it just returns the Vec.)

### 2. Protocol pin maps + CPU decoder — new `src/sim/models/bus_trace.rs`

Logical-pin tables per protocol:
- **APB3**: `psel penable pwrite pready pslverr paddr[] pwdata[] prdata[]`
- **AHB-Lite**: `htrans[1:0] haddr[] hwrite hsize[2:0] hburst[2:0] hready hresp hwdata[] hrdata[]`
- **AHB5**: AHB-Lite + optional `hnonsec hexcl hexokay hmaster[]` (resolved
  if present, ignored if absent)

Default net name `{prefix}{pin}` (lowercased), overridable via `signals`.
Resolution via the shared multi-candidate resolver (item 4).

`BusTraceDecoder` (per bus) consumes raw captured beats and emits:
```rust
pub struct BusTransaction {
    pub tick: u64, pub bus: String, pub protocol: BusProtocol,
    pub dir: Dir,            // Read | Write
    pub addr: u64, pub data: u64,
    pub resp: BusResp,       // Ok | Error
    pub burst: Option<BurstInfo>, // beat index / length for AHB
}
```
- **APB3 FSM**: GPU gates capture on `psel & penable & pready` (access-phase
  complete), so each captured beat *is* a complete transaction. `dir = pwrite`,
  `data = pwrite ? pwdata : prdata`, `resp = pslverr`.
- **AHB FSM**: GPU gates capture on `hready` high (pipeline advance) and records
  `htrans, haddr, hwrite, hsize, hburst, hwdata, hrdata, hresp`. CPU keeps a
  1-deep pending address-phase record and pairs address beat N with the data on
  beat N+1; tracks burst beat counter from `hburst`/`htrans==SEQ`.

Pure-Rust, unit-tested with synthetic beat sequences — no GPU required. This is
the testability win of CPU-side decode.

### 3. GPU capture — `csrc/kernel_v1.metal` + `src/sim/cosim_metal.rs`

Generalize the WbTrace structs into protocol-agnostic capture:
```c
#define MAX_BUS_TRACES 4
#define BUS_TRACE_MAX_ADR_BITS 32
#define BUS_TRACE_MAX_DAT_BITS 32

struct BusTraceParams {           // one per configured bus
    u32 protocol;                 // 0=apb3 1=ahb-lite 2=ahb5
    u32 gate_a_pos, gate_b_pos, gate_c_pos;   // edge-gating bits (psel/penable/pready or hready/htrans)
    u32 dir_pos, resp_pos;
    u32 addr_pos[BUS_TRACE_MAX_ADR_BITS];
    u32 wdata_pos[BUS_TRACE_MAX_DAT_BITS];
    u32 rdata_pos[BUS_TRACE_MAX_DAT_BITS];
    u32 ctrl_pos[8];              // htrans, hsize, hburst, hnonsec, ...
    u32 addr_bits, data_bits;
};
struct BusTraceEntry { u32 tick, flags, ctrl; u32 addr, wdata, rdata; };
struct BusTraceChannel { u32 write_head, capacity, current_tick, n_buses; /* entries follow */ };
```
The kernel computes the per-protocol gate, and on a gating edge packs one
`BusTraceEntry` (bus id in `flags` high bits). No FSM, no pairing on GPU.

`gpu_io_step` currently uses buffer slots 0–5 (UART + WbTrace). Add slots 6–7
for `BusTraceParams[]` + `BusTraceChannel`. Metal allows ≫8 buffers, so extend
the existing dispatch rather than adding a kernel.

Rust mirrors of the structs in `cosim_metal.rs` (next to `WbTraceParams`),
`build_bus_trace_params()` resolving pins for each configured bus, buffer
allocation sized `MAX_BUS_TRACES`, and a per-bus read head in the drain loop
(near `cosim_metal.rs:4057`) feeding each `BusTraceDecoder`.

### 4. Shared signal resolver — refactor `src/sim/trace_signals.rs`

Extract the multi-candidate name → AIG-pin / state-position resolver
(currently internal to trace-signal registration) into a reusable helper
callable from `build_bus_trace_params`. Keeps one source of truth for the
Yosys/scalar/structural naming conventions.

### 5. Output

- **CSV** (`--bus-trace-csv <PATH>`): drain-time, one row per `BusTransaction`.
  Header: `tick,bus,protocol,dir,addr,data,resp,burst`. Trivial — lands in
  Phase 1.
- **Annotated VCD**: synthesized per-bus VCD vars (`{bus}_addr`,
  `{bus}_wdata`/`{bus}_rdata`, `{bus}_dir`, `{bus}_resp`) that value-change at
  transaction-complete ticks. This needs a **new "virtual signal" emission
  path** in `vcd_io.rs`: unlike existing extra-observables (raw nets sampled
  per tick from the state buffer), these are sparse CPU-decoded events the VCD
  writer must interleave by tick. Bigger plumbing → **Phase 3**. Dovetails with
  the wire-bundle-scripting / Surfer direction in project memory.

### 6. CLI — `src/bin/jacquard.rs`

- `--bus-trace-csv <PATH>` (Phase 1)
- bus VCD annotation folded into the output/`--output-vcd` when `bus_traces` is
  configured, or a dedicated `--bus-trace-vcd` flag (Phase 3)

## Status

**Phase 1 is complete** (APB3 end-to-end + CSV). Validated by
`tests/apb_trace/` — a dedicated synthesized APB3 design (the Hazard3
JTAG-DM post-PnR netlist drops the APB addr/data nets during flattening,
so a names-preserved design was built instead). CI step:
`Run APB3 bus-trace cosim (Decision 0013)`. Phases 2–3 remain.

## Phasing

1. **Phase 1 — APB3 end-to-end.** ✅ Done. Config schema, pin maps, shared
   resolver, APB3 GPU capture, APB3 CPU decoder, CSV output. Validated on
   `tests/apb_trace/` (synthesized APB3 design). APB3 FSM unit-tested.
2. **Phase 2 — AHB-Lite + AHB5.** Pipeline pairing, burst tracking, AHB5 extra
   signals. Unit-test the AHB FSM. Needs an AHB design to integration-test
   against (open question — see below).
3. **Phase 3 — Annotated VCD.** Virtual-signal emission path in `vcd_io.rs`.
4. **Follow-up — migrate WbTrace** onto the general mechanism (express the
   VexRiscv ibus/dbus as configured buses), then delete the hardcoded path.

## Verification

- Unit: APB3 & AHB FSM decoders against synthetic beat vectors (pure Rust, no GPU).
- Integration (Phase 1): cosim the Hazard3 JTAG-DM with `--bus-trace-csv`,
  assert the expected DMI register accesses (DMCONTROL/DMSTATUS) appear.
- Build: `cargo build --release --features metal` clean; existing cosim tests
  (single-UART, WbTrace) unaffected since `bus_traces` defaults empty.

## Open questions

- **AHB integration test design.** APB3 validates on the existing Hazard3
  JTAG-DM. Phase 2 needs an AHB-Lite/AHB5 design — do we have one, or synthesize
  a small AHB peripheral (like `tests/dual_uart/`)?
- **Per-bus ring vs shared ring.** One `BusTraceChannel` with a bus-id field
  (simpler allocation) vs one ring per bus (no cross-bus contention). Start
  shared; revisit if a hot multi-bus design overflows.
- **CUDA/HIP.** Cosim is Metal-only today; no kernel changes needed elsewhere
  now, but the general design should port cleanly when CUDA cosim lands.

## decision impact

This generalizes the cosim peripheral architecture — update **decision-0013**
(plural-peripheral configs) to record the config-driven bus-monitor pattern and
the GPU-capture/CPU-decode split, once Phase 1 is real.
