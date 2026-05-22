# ADR 0011 — RAM port-mapping schema for declarative cell metadata

**Status:** Proposed.

## Context

[ADR 0010](0010-declarative-cell-metadata.md) shipped a minimal Tier 2
slice with one `kind` discriminator per cell. For `kind = "ram"`
specifically, v1.0 declares the cell-as-opaque: the AIG allocates a
`RAMBlock` slot but routes outputs to X-source slots without
resolving read/write port semantics. That's sufficient for "design
boots from ROM, never reads SRAM contents" cases but **fails the
moment a real CPU writes to SRAM and expects to read its data back**.

The acute trigger is the JTAG-DM firmware-load path enabled by
[PR #78](https://github.com/ChipFlow/Jacquard/pull/78): OpenOCD
walks a debug-module sequence that culminates in abstract-memory
writes into the design's SRAM, then jumps the CPU to that memory.
Because the SRAM is opaque (no backing storage, writes go nowhere),
the CPU boots to garbage. Issue
[#80](https://github.com/ChipFlow/Jacquard/issues/80) captures the
symptom and notes that wiring `SramInitConfig` is the smaller sibling
problem — pre-loading SRAM contents at tick 0 — but the bigger gap
is that `kind = "ram"` doesn't model writes at all.

ADR 0010 § "Deferred to a future ADR" listed the port-mapping
schema explicitly:

> Port-mapping schema (`[cells.NAME.ports]` sub-tables, polarity
> annotations, bus-width inference, write-enable encoding). This is
> a small behavioural description language doing more than
> classification; needs concrete adoption data before its schema is
> fixed.

The OCD GF180MCU SRAM (`gf180mcu_ocd_ip_sram__sram1024x8m8wm1`) — a
real third-party IP cell behind the apitronix-semiconductor /
hazard3 / future wafer.space tapeout pipelines — gives us the
concrete adoption-data input. This ADR fixes the schema against
that worked example.

## Worked example: the OCD SRAM

The upstream behavioural model
([RTimothyEdwards/gf180mcu_ocd_ip_sram](https://github.com/RTimothyEdwards/gf180mcu_ocd_ip_sram))
declares:

```verilog
module gf180mcu_ocd_ip_sram__sram1024x8m8wm1 (
    CLK, CEN, GWEN, WEN, A, D, Q
);
  input         CLK;                // posedge clock
  input         CEN;                // chip enable, active-low
  input         GWEN;               // global write enable, active-low
  input  [7:0]  WEN;                // per-bit write mask, active-low
  input  [9:0]  A;                  // address (1024 entries)
  input  [7:0]  D;                  // data in
  output [7:0]  Q;                  // data out
  reg    [7:0]  mem[1023:0];        // backing storage
```

Read semantics: on posedge `CLK`, when `!CEN && GWEN` → `Q = mem[A]`.
Write semantics: on posedge `CLK`, when `!CEN && !GWEN && !(&WEN)` →
`mem[A][i] = D[i]` for each `i` where `!WEN[i]`.

The schema needs to capture: per-pin polarity (active-low vs
active-high), per-pin role (clock / chip-enable / write-enable /
mask / address / data-in / data-out), bus widths (derived from the
Verilog declaration; not redeclared), mask granularity (per-bit vs
per-byte vs none).

## Decision

Extend the `<library>.cells.toml` schema with an optional `ram`
sub-table on entries declaring `kind = "ram"`. Presence of the
sub-table promotes a cell from **opaque** (v1.0 semantics) to
**explicit** — outputs are properly wired to the AIG-backed
`RAMBlock`, writes populate backing storage, reads return what was
written.

### Schema (v1.1)

```toml
schema_version = "1.1"

[cells.gf180mcu_ocd_ip_sram__sram1024x8m8wm1]
kind = "ram"

[cells.gf180mcu_ocd_ip_sram__sram1024x8m8wm1.ram]
depth = 1024
width = 8
clock        = { pin = "CLK", edge = "pos" }
chip_enable  = { pin = "CEN", polarity = "low" }
write_enable = { pin = "GWEN", polarity = "low" }
write_mask   = { pin = "WEN", polarity = "low", granularity = "bit" }
address      = "A"
data_in      = "D"
data_out     = "Q"
```

### Field semantics

- **`depth`** (required, integer): number of addressable entries.
  Must satisfy `depth ≤ 2^AIGPDK_SRAM_ADDR_WIDTH` (8192 today).
- **`width`** (required, integer 1..=32): bit-width of each entry.
  Capped at 32 by `RAMBlock`'s fixed-size port arrays.
- **`clock`** (required, table): `pin` is the clock input pin name;
  `edge` defaults to `"pos"`. `"neg"` is accepted (matches gf180mcu
  `dffnq`-family negedge convention).
- **`chip_enable`** (optional, table): `pin` + `polarity` (default
  `"low"`). When the pin's effective level is inactive, the cell
  neither reads nor writes for that cycle. Omit for sync SRAMs that
  are always-enabled.
- **`write_enable`** (optional, table): `pin` + `polarity` (default
  `"low"`). Gates **all** writes regardless of mask. The OCD SRAM's
  `GWEN`. Omit for SRAMs without a global write-enable.
- **`write_mask`** (optional, table): per-bit / per-byte write
  enables. `pin` is the mask pin name; `polarity` defaults to
  `"low"`; `granularity` is `"bit"` (default) or `"byte"`. The mask
  width must match `width` (bit) or `width / 8` (byte). Omit for
  SRAMs without per-bit masking — in that case the global
  `write_enable` controls the whole word.
- **`address`** / **`data_in`** / **`data_out`** (required,
  string): pin names. Bus widths are read from the Verilog (via
  `sverilogparse`) — not re-declared here.

### Optional cells (no `ram` block)

Cells declaring `kind = "ram"` *without* the `ram` sub-table fall
back to v1.0 **opaque mode** — outputs route to X-source slots, no
backing storage, no port resolution. The contract is unchanged for
existing consumers.

### Backing storage

Cells with an explicit `ram` block allocate a `RAMBlock` with
`port_r_*` and `port_w_*` arrays populated from resolved pin
positions. The simulator's existing GPU-side SRAM machinery handles
reads, writes, and per-entry backing memory; no new kernel work is
required.

### Schema versioning

The top-level `schema_version` field bumps from `"1.0"` to `"1.1"`.
v1.0 manifests continue to parse — the `ram` sub-table is purely
additive. Loaders that don't recognise the new sub-table (none
today; this ADR ships the loader simultaneously) would treat
flagged cells as opaque RAMs, which is a graceful degradation.

### SRAM preload (sibling work)

`TestbenchConfig::sram_init` (an existing schema field declared in
`src/testbench.rs` but unwired today — issue
[#80](https://github.com/ChipFlow/Jacquard/issues/80)) becomes
load-bearing once explicit-port RAMs have backing storage. The
preload path:

1. Parse ELF segments from `sram_init.elf_path`.
2. Match segments to SRAM instances by virtual-address overlap with
   declared SRAM regions.
3. Write segment bytes into each matched SRAM's backing memory at
   tick 0.

Schema extensions to `SramInitConfig` (instance targeting,
multi-section support) land alongside the implementation but don't
require an ADR — purely additive JSON schema work.

## Consequences

- The OCD GF180MCU SRAM (and any structurally similar third-party
  IP — 1RW, sync, optional per-bit mask) becomes simulable end-to-end
  via the manifest pathway. Real CPU writes populate real memory.
- The opaque-mode fallback stays load-bearing for cells the consumer
  hasn't taken the time to schema-map — important so the cell-library
  pathway doesn't *require* schema work just to load a cell library.
- JTAG-DM-driven firmware load (PR #78 stage 1) becomes end-to-end
  testable in cosim. Closes the chicken-and-egg loop for designs
  whose firmware-load mechanism is what cosim is trying to validate.
- The schema is opinionated: 1-port (1RW), sync-only, write-mask is
  bit OR byte (not arbitrary). Multi-port SRAMs (`2RW`, `1R1W`),
  async SRAMs, and write-mask-with-stripes encodings are explicitly
  out of scope. Adding them is a future schema version (v1.2+);
  doesn't break v1.1 manifests.

## Out of scope

- **Multi-port SRAMs.** Most foundry IPs in our ecosystem are
  single-port. Dual-port designs are a meaningful follow-up but
  not driven by any in-tree fixture today.
- **Async (non-clocked) SRAMs.** Hardly seen in synthesised
  digital designs at modern PDKs. Not modeled.
- **Width > 32 bits.** Bounded by `RAMBlock`'s array sizes;
  consumers wider than 32 should split into multiple instances.
- **Built-in classifier removal.** Same rule as ADR 0010 — the
  `$__RAMGEM_SYNC_` and `CF_SRAM_*` recognisers stay as fallback;
  manifest-declared RAMs supplement, don't replace.

## Links

- ADR 0010 — Declarative cell metadata (the parent decision deferring
  this schema).
- Issue [#80](https://github.com/ChipFlow/Jacquard/issues/80) —
  driving consumer.
- PR [#78](https://github.com/ChipFlow/Jacquard/pull/78) — the
  JTAG-DM workflow that surfaced the schema need.
- Upstream OCD SRAM behavioural model:
  [RTimothyEdwards/gf180mcu_ocd_ip_sram](https://github.com/RTimothyEdwards/gf180mcu_ocd_ip_sram).
