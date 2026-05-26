# Cosim Peripheral Models

Architecture: [ADR 0013](../adr/0013-plural-peripheral-configs.md).

This plan tracks implementation work for the cosim peripheral model
framework. ADR 0013 documents the architecture (two execution domains,
observe-only vs bidirectional GPU patterns, ring buffers, plural config
convention); this doc tracks the concrete workstreams.

## Phase 1: Multi-UART ([#90](https://github.com/gpu-eda/Jacquard/issues/90))

First peripheral using the plural-config + array-in-kernel conventions
from ADR 0013.

### Schema — `src/testbench.rs`

Add `name: Option<String>` to `UartConfig`. Add
`uarts: Vec<UartConfig>` to `TestbenchConfig`. Add
`effective_uarts()` mirroring `effective_clocks()`:

```rust
pub fn effective_uarts(&self) -> Vec<UartConfig> {
    let mut out = self.uarts.clone();
    if let Some(ref u) = self.uart {
        out.insert(0, u.clone());
    }
    out
}
```

Existing `"uart": {...}` configs work unchanged. New form:
`"uarts": [{"name": "console", ...}, {"name": "debug", ...}]`.
Both may coexist; `uart` is prepended to `uarts`.

### Metal kernel — `csrc/kernel_v1.metal`

`MAX_UARTS = 4`. Restructure the three UART types:

```c
#define MAX_UARTS 4

struct UartPerChannelConfig {
    u32 tx_out_pos;
    u32 cycles_per_bit;
};

struct UartParams {
    u32 state_size;
    u32 n_uarts;          // replaces has_uart
    u32 _pad[2];
    UartPerChannelConfig channels[MAX_UARTS];
};
```

`UartDecoderState` and `UartChannel` structs unchanged — the device
buffers hold `[MAX_UARTS]` elements. `gpu_io_step` buffer signature
unchanged (same 6 slots); the UART decode block becomes a loop over
`n_uarts`.

### Rust runtime — `src/sim/cosim_metal.rs`

- **Repr structs** (~line 130): update `UartParams` to match kernel.
  Add `UartPerChannelConfig`. Keep `UartDecoderState` and
  `UartChannel` unchanged.
- **Config resolution** (~line 2229): iterate `effective_uarts()`.
- **Buffer allocation** (~line 2820): size buffers for `MAX_UARTS`
  elements. Init each `UartDecoderState` with `last_tx=1`.
- **RX driver creation** (~line 2544): one `UartRxDriver` per entry,
  named `uart_{name}` (fallback `uart_{index}`).
- **CPU drain** (~line 3990): iterate N channels with per-channel
  `uart_read_head[i]`. Label events with UART name.

### Verification

- `cargo build --release --features metal` compiles.
- `cargo test --lib` passes (add `effective_uarts` unit tests).
- Existing MCU SoC cosim CI passes unchanged (single `"uart"` config).
- Local smoke: temporarily edit `tests/jtag_minimal/sim_config.json`
  to use `"uarts": [...]` syntax, confirm identical results.

### Not in scope

- Dual-UART test fixture: separate follow-up with a small 2-TX design.
- CUDA/HIP: cosim is Metal-only; no kernel changes needed.

## Future phases

| Phase | Scope | Status |
|---|---|---|
| 2 | Refactor `gpu_io_step` toward common params/ring-buffer layout | Future |
| 3 | Multi-Flash / external RAM (bidirectional pattern) | Deferred (no use case) |
| — | Multi-JTAG | Not needed (TAP daisy-chain suffices) |
