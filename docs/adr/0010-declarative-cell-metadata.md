# ADR 0010 — Declarative cell metadata for PDK enablement

**Status:** Accepted.

## Context

PDK enablement today is *per-PDK code + vendored Verilog* (see
`src/sky130.rs`, `src/gf180mcu.rs`, `src/gf180mcu_pdk.rs`, the
`build.rs` pin-table scanner). Adding a new cell family — third-party
IP memories, hard macros, foundry-supplied blocks — requires
vendoring Verilog into `jacquard/vendor/`, extending the `build.rs`
scanner, editing prefix matchers (`is_<pdk>_cell`,
`extract_cell_type`), and adding entries to hand-curated `matches!()`
lists (`is_filler_cell`, `is_io_pad_cell`, `is_sequential_cell`,
`is_multi_output_cell`, …). Each of those last is data masquerading
as code; PR #64 (2026-05-18 power-pin + wired-filler shortcuts for
wafer.space) is the most recent example of the pattern.

The acute trigger is `gf180mcu_ocd_ip_sram__sram1024x8m8wm1` — Tim
Edwards' OCD 3.3V port of the GF180MCU SRAM IP, used in a downstream
wafer.space tapeout. The cell is third-party IP (not in Jacquard's
`vendor/`), doesn't match `is_gf180mcu_cell`'s prefix walk
(`fd_*` / `ws_*` only), has no pin table, and isn't filler-stubbable.
Issue [#67](https://github.com/ChipFlow/Jacquard/issues/67) captures
the discussion.

The same pattern will repeat for every wafer.space tapeout that
includes IP outside Jacquard's vendored library — hazard3, future
chips. Code-gating each one through a Jacquard PR doesn't scale.

## Decision

PDK enablement gains a **declarative metadata path** alongside the
existing built-in classifiers. The decision separates cleanly into
two tiers; this ADR commits to Tier 1 + a minimal Tier 2 slice now,
and explicitly defers the larger Tier 2 schema (port-mapping
semantics) to a future ADR after real adoption data.

### Tier 1 — runtime cell library (`--cell-library <PATH>.v`)

`sverilogparse` (already a workspace dependency) parses user-supplied
Verilog files at startup and populates the `LeafPinProvider` for
every `module … endmodule` block found. Handles `input` / `output` /
`inout`. Replaces the `build.rs` scanner for newly-added cells;
existing built-in tables stay as fallback.

Flag is **repeatable**: `--cell-library a.v --cell-library b.v` for
designs that pull in multiple IP libraries. Files are parsed in
order; later files override earlier ones for collisions (with a
warning).

### Tier 2 (minimal slice) — `kind` discriminator in TOML

Each cell library may be accompanied by a TOML manifest declaring
the *kind* of each cell — the same classification today's
`is_filler_cell` / `is_sequential_cell` / etc. encode in
`matches!()` lists. Manifest path mirrors the library path
(`foo.v` → `foo.cells.toml`) and is loaded automatically when
present; an explicit `--cell-manifest <PATH>.toml` flag overrides
the autoloading behaviour.

```toml
schema_version = "1.0"

[cells.gf180mcu_ocd_ip_sram__sram1024x8m8wm1]
kind = "ram"

[cells.gf180mcu_fd_io__fillcap_18_h]
kind = "filler"
```

Recognised `kind` values (v1.0): `std`, `dff`, `latch`,
`clock_gate`, `ram`, `filler`, `endcap`, `tap`, `io_pad_input`,
`io_pad_output`, `io_pad_bidir`, `delay`, `multi_output`,
`tie_high`, `tie_low`.

Schema versioning: top-level `schema_version` is mandatory. v1.x
additive rule — new optional keys / new `kind` values are
non-breaking; semantics of existing `kind` values must not narrow.

### `kind = "ram"` semantics in v1.0 (opaque-RAM mode)

`aig.rs` today has two hardcoded RAM detection paths:
`celltype == "$__RAMGEM_SYNC_"` (line 775, port_r/port_w resolution
from Yosys `memlib_yosys.txt`) and `starts_with("CF_SRAM_")` (line
1006, `.DO` output resolution for ChipFlow's single-port
convention). Neither matches `gf180mcu_ocd_ip_sram_*` or arbitrary
third-party SRAM IP.

In v1.0, `kind = "ram"` allocates a `RAMBlock` slot in **opaque
mode**: the cell's outputs are routed to X-source slots, no port
resolution is attempted, no memory behaviour is modelled. This is
sufficient for designs whose CPU executes from boot ROM / register
file and never reads SRAM contents at the timescales Jacquard
simulates (the heartbeat-verification use case driving this work).
The existing `compute_x_sources` test path at `src/aig.rs:3247-3273`
already validates the X-source convergence shape.

When real memory modelling is required, future schema versions add
explicit port mapping (`[cells.NAME.ports]` sub-tables) — the
opaque mode stays as the documented fallback.

### Integration ordering

`aig.rs` cell-type recognition slots the manifest path **after** the
existing recognisers:

```text
1. celltype == "$__RAMGEM_SYNC_"  → RAMBlock with port_r/port_w   (unchanged)
2. starts_with("CF_SRAM_")        → RAMBlock with .DO              (unchanged)
3. PdkVariant::classify(celltype) → built-in classifier dispatch    (unchanged)
4. NEW: manifest.lookup(celltype) → manifest-declared kind dispatch
```

The new path activates only for cells none of the existing
recognisers match AND that have a manifest entry. All existing
tests stay green without churn.

### Deferred to a future ADR

- **Port-mapping schema** (`[cells.NAME.ports]` sub-tables, polarity
  annotations, bus-width inference, write-enable encoding). This is
  a small behavioural description language doing more than
  classification; needs concrete adoption data before its schema is
  fixed.
- **Built-in classifier removal.** `sky130.rs` /
  `gf180mcu.rs` / `gf180mcu_pdk.rs` classification tables stay as
  fallback through the entire migration. Removal happens only after
  the manifest pathway is the source of truth for at least one PDK
  in production.
- **`build.rs` pin-table scanner removal.** Same rule: removed LAST,
  after manifests cover the built-in PDKs.

## Consequences

- Third-party IP unblocks without Jacquard PRs. Users ship a
  `<library>.cells.toml` alongside their `<library>.v`; CI flows
  point `--cell-library` at both. The driving wafer.space tapeout's
  `chip_top.pnl.v` clears `gf180mcu_ocd_ip_sram__sram1024x8m8wm1`
  by shipping a six-line manifest entry.
- The "vendor + edit code + extend lists" PR workflow for new IP
  becomes "ship a manifest, no Jacquard change". `docs/adding-a-pdk.md`
  evolves to document the manifest pathway as the primary route.
- The opaque-RAM semantics is honest about what v1.0 delivers — no
  silent partial memory modelling. The contract is "RAMBlock
  allocated, outputs X-source, no read/write behaviour" until a
  future schema version adds explicit ports.
- Existing built-in PDK code stays load-bearing through the
  transition. No risk of regression in sky130 / gf180mcu test
  flows during the migration.

## Links

- Issue [#67](https://github.com/ChipFlow/Jacquard/issues/67) —
  design discussion.
- PR #64 (`9281e57`) — most recent per-PDK-code-as-data workaround
  this ADR resolves.
- ADR 0009 — OpenSTA Verilog reader input constraints
  (`sverilogparse` is already in-tree for unrelated reasons; Tier 1
  reuses that dep).
- `docs/plans/declarative-cell-metadata.md` — implementation phasing.
- `docs/plans/gf180mcu-enablement.md` § Follow-on cleanup items
  1, 2, 3 — superseded by this ADR.
