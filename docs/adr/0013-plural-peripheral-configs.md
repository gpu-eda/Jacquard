# ADR 0013 — Cosim peripheral model architecture

**Status:** Proposed

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

Current CPU-side models: GPIO, UART RX, JTAG replay, I²C, SPI.

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
| Flash | `flash: Option<FlashConfig>` | Not yet |
| JTAG | `jtag: Option<JtagConfig>` | Not yet |
| Wishbone | *(auto-detected from netlist)* | N/A |

### Current implementation (bespoke kernels)

Today each GPU-side peripheral has its own kernel function:

| Kernel | Slots | Pattern |
|---|---|---|
| `gpu_apply_flash_din` | states[0], flash_state[1], flash_din_params[2] | Bidirectional: inject |
| `gpu_flash_model_step` | states[0], flash_state[1], flash_model_params[2], flash_data[3] | Bidirectional: sample+advance |
| `gpu_io_step` | states[0], uart_state[1], uart_params[2], uart_channel[3], wb_channel[4], wb_params[5] | Observe-only (combined UART + Wishbone) |

All run on thread 0 only — the per-tick work is a trivial FSM step.
`gpu_io_step` combines two logically independent observe-only models,
gated by `n_uarts > 0` and `has_trace` flags.

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
| 2 | Refactor `gpu_io_step` to use common params/ring-buffer layout | Future |
| 3 | Multi-Flash / external RAM (bidirectional pattern) | Deferred (no use case yet) |
| — | Multi-JTAG | Not needed (TAP daisy-chain suffices) |

Plan doc: [`../plans/multi-peripheral-cosim.md`](../plans/multi-peripheral-cosim.md).
