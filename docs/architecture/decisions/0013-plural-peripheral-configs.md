# Decision 0013 — Cosim peripheral model architecture

**Status:** Accepted — the architecture is implemented and in use across

**Current architecture:** [Cosim runtime](../cosim-runtime.md).
multiple peripherals (multi-UART #90, config-driven APB3 bus tracing).
The "Target architecture" section below tracks the remaining, optional
refactors; the conventions it establishes are already followed.

> **Amendment (2026-06-25):** Two body claims are now stale. (1) "Cosim is
> Metal-only today" — CUDA (`src/sim/cosim/cuda.rs`) and HIP
> (`src/sim/cosim/hip.rs`) backends now implement the full peripheral stack
> (`gpu_io_step`, flash FSM, ring-buffer drain); the "Metal-only"
> characterisation applied only to the *prebuilt distribution* (Decision 0018),
> not the source build. (2) The note that `step_edge` receives an empty
> `output_state` slice was resolved by the interactive JTAG work — it now
> receives `&backend.state()[state_size..]` (`cosim/mod.rs`). The original
> text below is retained for the record.

> **Amendment (2026-07-09):** Plural QSPI memories may now share one SCK/SIO
> bus, selected by distinct CS lines (config: same `clk_gpio`/`d0_gpio`,
> distinct `csn_gpio`). This requires CS-gated MISO injection: a deselected
> instance (`prev_csn` high) presents high-Z and must not drive. Previously
> `gpu_apply_flash_din` (and the CpuBackend mirror) wrote every instance's
> `d_i` to its `d_in_pos` unconditionally, so on a shared bus the
> last-iterated (typically deselected) memory clobbered the selected driver.
> The gate lives in `gpu_apply_flash_din` (Metal + the shared CUDA/HIP
> `kernel_v1_impl.cuh`) and `CpuBackend::apply_flash_din_gated`; regression:
> `tests/qspi_shared_bus/` (golden + content, `all`/`qspi` scopes) plus the
> `flash_din_tests` unit tests. Independent-pin plurality (the original Stage
> B/C design, `tests/multi_mem_cosim/`) is unaffected — a deselected flash on
> its own pins was always ignored by the design; the gate only changes the
> now-shared case.

## Context

Jacquard's cosim mode runs reactive peripheral models alongside the
GPU-simulated design: SPI flash serves firmware, UART decodes serial
output, JTAG replays debug sessions, GPIO drives/observes pins, and
Wishbone trace captures bus transactions. The architecture evolved
organically; this ADR documents the current design, identifies the
abstractions emerging from it, and establishes conventions for
extending it.

## Architecture

### Execution domains

Peripheral work splits across CPU and GPU. The boundary follows a
simple rule: models that **drive input pins** (must react to design
output each edge) run on the CPU; models that **observe output pins**
(pure consumers of post-simulation state) or **exchange data
bidirectionally** with the design run on the GPU for zero-copy access
to the state buffer.

Some peripherals span both domains. UART has a CPU-side RX driver
(feeds bytes into the design's RX input pin) and a GPU-side TX decoder
(reads the design's TX output pin).

### CPU-side: `PeripheralModel` trait

Defined in `src/sim/models/mod.rs`:

```rust
trait PeripheralModel {
    fn name(&self) -> &str;
    fn driven_positions(&self) -> &[u32];
    fn apply_action(&mut self, action: &QueuedAction);
    fn step_edge(&mut self, output_state, overrides, emitted); // default: just calls contribute_overrides
    fn contribute_overrides(&self, overrides);
    fn is_active(&self) -> bool; // default: false
}
```

`apply_action` is how the `InputDispatcher` feeds queued stimulus
commands to models. `is_active` signals that the model is mid-
transmission and needs per-edge granularity (forces batch size to 1).
`step_edge` has a default that just calls `contribute_overrides` —
stateless models (GPIO) only need the latter.

Models are registered into a `Vec<Box<dyn PeripheralModel>>` at
startup. Each batch boundary, the loop calls `step_edge` on every
model; models write their pin drives into a shared `ModelOverrides`
map. These overrides are patched in-place into pre-allocated `BitOp`
arrays (built at startup with placeholder entries for model-driven
positions) and applied via the `state_prep` GPU kernel.

**Note:** `step_edge` currently receives an empty `output_state`
slice — GPU output state is not read back per-edge for CPU-side
models. GPIO and UART RX don't need it; I²C and SPI bus observation
will require wiring the output state readback when those models are
completed.

The dispatch is peripheral-agnostic: `state_prep` applies whatever
`BitOp` array it receives. Clock edges, reset, GPIO, UART RX, and
JTAG TCK/TMS/TDI are all entries in the same ops buffer.

Registered CPU-side models: GPIO, UART RX, JTAG replay (complete);
I²C, SPI (scaffolded, output-state readback not yet wired).

### GPU-side: two model patterns

GPU-side models fall into two categories distinguished by their
data-flow relationship to the simulation:

**Observe-only (post-simulate):** The model reads output state after
simulation and produces results (decoded bytes, bus traces) into a
ring buffer. It never writes to input state. One kernel call per
edge, after `simulate_v1_stage`.

**Bidirectional (pre+post simulate):** The model both reads the
design's outputs *and* injects data into the design's inputs. This
requires two kernel calls per edge — one before simulation (inject
response data into input state) and one after (read request signals
from output state, advance the model's FSM).

| Pattern | When | Current models |
|---|---|---|
| Observe-only | Post-simulate | UART TX decoder, Wishbone bus trace |
| Bidirectional | Pre-simulate (inject) + post-simulate (sample, advance) | SPI Flash |

Any memory-mapped peripheral (external SRAM, I²C EEPROM, etc.)
would follow the bidirectional pattern.

### Per-edge execution order

```
state_prep (apply clk/gpio/jtag pin drives from CPU-side models)
  → [bidirectional: inject] — e.g. gpu_apply_flash_din
    → simulate_v1_stage ×N (combinational logic evaluation)
  → [bidirectional: sample+advance] — e.g. gpu_flash_model_step
  → [observe-only] — e.g. gpu_io_step (UART TX + Wishbone)
```

CPU-side `PeripheralModel::step_edge` runs between GPU batches.

### GPU→CPU communication: ring buffers

GPU-side models produce output into fixed-size ring buffers in device
memory. The CPU drains these after each GPU batch completes, reading
from a local `read_head` up to the GPU-written `write_head`. No
synchronisation beyond Metal's command buffer completion is needed.

Current ring buffers:

| Buffer | Element | Capacity |
|---|---|---|
| `UartChannel` | `u8` (decoded bytes) | 4096 |
| `WbTraceChannel` | `WbTraceEntry` (20 bytes) | 16384 |

### Configuration

Peripheral config lives in `sim_config.json`, deserialized into
`TestbenchConfig` (`src/testbench.rs`):

| Peripheral | Field | Plural? |
|---|---|---|
| Clock | `clocks: Option<Vec<ClockConfig>>` | Yes (`effective_clocks()`) |
| GPIO | `gpios: Vec<GpioConfig>` | Yes |
| UART | `uart` + `uarts: Vec<UartConfig>` | Yes (`effective_uarts()`, #90) |
| QSPI memory (flash/PSRAM) | `flash` + `qspi_memory: Vec<QspiMemoryConfig>` | Yes (`effective_qspi_memory()`, #170/#171) |
| JTAG | `jtag: Option<JtagConfig>` | Not yet |
| Wishbone | *(auto-detected, hardcoded signal names)* | N/A (legacy) |
| Bus trace (AHB/APB) | `bus_traces: Vec<BusTraceConfig>` | Yes (`effective_bus_traces()`) |

### Current implementation (bespoke kernels)

Today each GPU-side peripheral has its own kernel function:

| Kernel | Slots | Pattern |
|---|---|---|
| `gpu_apply_flash_din` | states[0], flash_state[1], flash_din_params[2] | Bidirectional: inject |
| `gpu_flash_model_step` | states[0], flash_state[1], flash_model_params[2], flash_data[3] | Bidirectional: sample+advance |
| `gpu_io_step` | states[0], uart_state[1], uart_params[2], uart_channel[3], wb_channel[4], wb_params[5], bus_channel[6], bus_params[7] | Observe-only (UART + Wishbone + AHB/APB bus trace) |

All run on thread 0 only — the per-tick work is a trivial FSM step.
`gpu_io_step` combines three logically independent observe-only models,
gated by `n_uarts > 0`, `has_trace`, and `n_buses > 0` respectively.

### Config-driven bus monitor (AHB/APB)

The Wishbone trace (`build_wb_trace_params`) hardcodes one SoC's signal
names (`cpu.fetch.ibus__cyc`, `spiflash.ctrl.wb_bus__ack`, …) directly
in source. The AHB/APB bus tracer generalizes it into a **config-driven,
protocol-aware monitor** that is the model for future bus tracing:

- **Config** (`BusTraceConfig`): `name`, `protocol` (apb3 / ahb-lite /
  ahb5), hierarchical `prefix`, `addr_bits`/`data_bits`, and optional
  per-pin `signals` overrides. Pins default to `{prefix}{pin}`.
- **Pin binding**: protocol pin names (`psel`, `paddr`, …) are resolved
  to output-state positions via `resolve_to_state_pos` in
  `trace_signals.rs` — the same multi-candidate resolver `--trace-signals`
  uses, so Yosys-flattened / scalar-expanded / structural naming all
  work. The pins are registered as observables before partitioning (via
  `DesignArgs::extra_observable_signals`) so they get state-buffer slots.
- **GPU capture / CPU decode split**: the kernel is protocol-agnostic —
  it packs a raw beat (`addr, wdata, rdata, ctrl flags`) into the ring
  buffer on the protocol's gating edge (`psel & penable & pready` for
  APB), using rising-edge detection so exactly one beat is recorded per
  completed transfer. The protocol FSM (phase pairing, burst tracking,
  response decode) lives in plain, unit-testable Rust in
  `src/sim/models/bus_trace.rs`. APB3 is stateless (one beat = one
  transaction); AHB pairing is the Phase-2 extension.
- **Output**: decoded transactions stream to CSV via `--bus-trace-csv`;
  annotated-VCD emission is a planned follow-up.

This is observe-only, so it slots into the existing post-simulate
pattern. Migrating the hardcoded WbTrace onto this mechanism (expressing
the VexRiscv ibus/dbus as configured buses) is a clean follow-up.

## Target architecture

The two patterns (observe-only, bidirectional) and the common
conventions (ring buffers, params structs, per-instance config arrays)
should be codified so new peripherals follow a template:

### Common conventions

- **Params struct layout:** `{ u32 state_size; u32 n_active; u32 _pad[2]; PerInstanceConfig configs[MAX_N]; }` — uniform header, compile-time `MAX_N` cap.
- **Ring buffer struct:** `{ u32 write_head; u32 capacity; u32 _pad[2]; T data[CAP]; }` — shared across all models producing GPU→CPU output.
- **Buffer sizing:** always `MAX_N` elements regardless of `n_active`. Wastes negligible memory for small N.
- **Guard pattern:** `for (i = 0; i < n_active && i < MAX_N; i++)` replaces the current `has_foo != 0` booleans.

### Model registration

New GPU-side models declare which pattern they follow:

- **Observe-only:** register a post-simulate kernel. Receives output state (read-only), writes to ring buffer.
- **Bidirectional:** register a pre-simulate kernel (inject into input state) and a post-simulate kernel (read output state, advance FSM).

Today this registration is implicit in `cosim_metal.rs`'s
`encode_and_commit_gpu_batch`. Formalizing it is a future step —
the convention is sufficient while the model count is small.

### Plural config convention

To support multi-instance peripherals (multiple UARTs, potentially
multiple flash chips or RAM banks):

- Legacy singular field kept via `#[serde(default)]`.
- New plural field alongside (e.g. `uarts: Vec<UartConfig>`).
- `effective_<peripheral>() -> Vec<Config>` merges both.
- Each config struct gains `name: Option<String>` for labelling.

This mirrors the existing `effective_clocks()` pattern.

### Cross-backend considerations

Cosim is Metal-only today. CUDA/HIP paths (`kernel_v1_impl.cuh`)
implement the core simulation kernel but have no `gpu_io_step` or
flash kernels. When CUDA/HIP cosim is added, the same two-pattern
taxonomy applies — the kernel implementations will differ but the
Rust-side buffer allocation, config resolution, and drain logic can
be shared via feature-gated code in `cosim_metal.rs` (or a
future `cosim_common.rs`).

## Phasing

| Phase | Scope | Status |
|---|---|---|
| 1 | Multi-UART ([#90](https://github.com/gpu-eda/Jacquard/issues/90)): first peripheral using plural-config + array-in-kernel conventions | **Done** |
| 1b | Config-driven bus monitor, APB3 + CSV (GPU-capture/CPU-decode split) | **Done** |
| 2 | Refactor `gpu_io_step` to use common params/ring-buffer layout | Future |
| 2b | AHB-Lite / AHB5 bus tracing + annotated-VCD output; migrate WbTrace onto the general monitor | Future |
| 3 | Multi-Flash / external RAM (bidirectional pattern) | **Done** — plural QSPI memory (`Vec<QspiMemoryConfig>`, N-instance kernels on Metal + CUDA/HIP) [#170](https://github.com/gpu-eda/Jacquard/pull/170)/[#171](https://github.com/gpu-eda/Jacquard/pull/171); writable QSPI PSRAM (RAM mode) [#159](https://github.com/gpu-eda/Jacquard/pull/159). See 2026-07-06 amendment. |
| — | Multi-JTAG | Not needed (TAP daisy-chain suffices) |

Plan docs: [`../plans/multi-peripheral-cosim.md`](../../plans/multi-peripheral-cosim.md),
[`../plans/bus-transaction-tracing.md`](../../plans/bus-transaction-tracing.md).

## Amendment 2026-06-21: interactive JTAG debug server config surface

The interactive JTAG/DM debug server (`--jtag-server`,
[#124](https://github.com/gpu-eda/Jacquard/issues/124)) is the live-socket
sibling of `--jtag-replay`. Its config additions follow this ADR's conventions;
the execution-model decisions are in
[Decision 0017's 2026-06-21 amendment](0017-cosim-execution-model.md). Recorded here:

- **`JtagConfig` gains `tdo_gpio: Option<usize>`** (`src/testbench.rs`). TDO is
  a design **output**, so unlike the existing `tck/tms/tdi/trst_gpio` (inputs,
  resolved via `input_bits`) it resolves via the **output-bit** map. It is the
  first JTAG pin that the model *observes* rather than *drives* — added
  `Option` so replay configs without it keep working.

- **Resolves the "`output_state` readback not yet wired" note above** for the
  JTAG case. The interactive server is the first CPU-side `PeripheralModel` that
  must read a design output (TDO, to answer `remote_bitbang` `R`), so it wires
  the real output slice into `step_edge` — the same plumbing the scaffolded
  I²C/SPI models were noted as needing. See Decision 0017's amendment for the
  execution-model detail.

- **New CLI flag `--jtag-server <PORT>`**, mutually exclusive with
  `--jtag-replay`. Reuses the same `jtag` peripheral pin mapping and
  `--jtag-hold-cycles` semantics; opens a `remote_bitbang` TCP server and drives
  the configured pins live from the connected OpenOCD/gdb client. JTAG stays in
  the "Not yet" plural column (TAP daisy-chain suffices — single instance).

Plan: [`../plans/jtag-debug-server.md`](../../plans/jtag-debug-server.md).

## Amendment 2026-07-06: plural QSPI memory + writable QSPI PSRAM (Phase 3 done)

The Flash peripheral went plural and gained a writable RAM mode, completing
Phase 3. This resolves the `plural-qspi-memory` working handoff (folded here).

- **Plural config (Stage A, [#170](https://github.com/gpu-eda/Jacquard/pull/170)).**
  `flash: Option<FlashConfig>` → **`qspi_memory: Vec<QspiMemoryConfig>`** with the
  standard back-compat pattern: the legacy `flash` key folds into instance 0 via
  `effective_qspi_memory()` (mirrors `effective_uarts()`). `FlashConfig` is now a
  type alias of `QspiMemoryConfig`. Each instance carries its own GPIO map, backing
  size, and (Stage C) firmware.

- **N-instance GPU kernels (Stage B, [#171](https://github.com/gpu-eda/Jacquard/pull/171)).**
  `FlashDinParams`/`FlashModelParams` are wrapped in `*All { u32 n_flashes; u32
  _pad[3]; T[MAX_QSPI_MEMS] }` blocks (exactly `BusTraceParamsAll`, the "target
  architecture" convention above). `FlashModelParams` gains a per-instance
  `data_offset`; `flash_data` is **one concatenated buffer** with each instance's
  store at its offset (independent backing stores). `FlashState` is an N-slot array;
  both kernels loop `f < n_flashes` instead of the old `has_flash` guard. Mirrored
  across Metal (`kernel_v1.metal`) and the shared CUDA/HIP kernel
  (`kernel_v1_impl.cuh`). Gotcha fixed in-flight: `flash_set_in_reset` must drive
  **every** slot's reset line, not just slot 0.

- **Writable QSPI PSRAM / RAM mode ([#159](https://github.com/gpu-eda/Jacquard/pull/159)).**
  Opt-in `FlashConfig` fields (`writable`, `enter_qpi_cmd`, `quad_write_cmd`,
  `read_dummy_cycles`, `size_bytes`; `firmware` now optional) turn an instance into
  an APS6404L-class PSRAM: enter-QPI (0x35) latches 4-lane command sampling, quad-
  write (0x38) stores into the now-writable backing store, quad-read (0xEB) inserts
  `3 + read_dummy/2` dummy boundaries. Mirrored across the CPU `CppSpiFlash` oracle,
  Metal, and CUDA/HIP with `#[repr(C)]` size asserts in lockstep (`FlashState` 52B,
  `FlashModelParams` 52B); the CUDA/HIP `flash_data` buffer became a writable
  `UnsafeCell<UVec<u8>>`. Unset options ⇒ byte-identical to the read-only flash.

- **Tests.** `tests/multi_mem_cosim/` (3 flashes + 2 SRAMs, independent stores,
  `all` scope) and `tests/qspi_psram/` (write→read round-trip, `all` + a dedicated
  `qspi` scope for the GPU suite). Goldens captured on CpuBackend, byte-identical to
  Metal/CUDA/HIP (Backend Equivalence CI green). Motivating use case: a C64-subset
  GF180 tapeout whose main RAM is external QSPI PSRAM.
