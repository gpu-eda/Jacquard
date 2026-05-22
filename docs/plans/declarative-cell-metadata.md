# Declarative cell metadata — Tier 1 + minimal Tier 2 + port mapping

**Status:** Implemented — historical record. Tier 1, minimal Tier 2,
and the port-mapping schema have all landed.
**ADRs:**
- [0010 — Declarative cell metadata for PDK enablement](../adr/0010-declarative-cell-metadata.md)
  (Tier 1 + minimal Tier 2)
- [0011 — RAM port-mapping schema](../adr/0011-ram-port-mapping-schema.md)
  (the port-mapping extension originally deferred by ADR 0010)
**Issues:** [#67](https://github.com/ChipFlow/Jacquard/issues/67),
[#80](https://github.com/ChipFlow/Jacquard/issues/80).
**Driving designs:** the wafer.space `chip_top.pnl.v` blocked on
`gf180mcu_ocd_ip_sram__sram1024x8m8wm1`, then the JTAG-DM workflow
in PR #78 surfacing the need for real RAM backing storage.

## Scope (as shipped)

Originally scoped to one slice (Tier 1 + minimal Tier 2 — opaque
`kind = "ram"` with no port resolution). Expanded mid-flight when
the JTAG-DM workflow (PR #78) surfaced the need for explicit-port
RAMs with real backing storage:

- **Tier 1**: `--cell-library` + sverilogparse-backed pin tables
  (landed 2026-05-19 in PR #65/#68).
- **Tier 2 minimal**: `kind` discriminator in TOML, opaque-RAM
  mode (landed alongside Tier 1).
- **Port-mapping schema** (ADR 0011, v1.1): `[cells.NAME.ram]`
  sub-table for explicit-port RAMs with backing storage. Landed
  in this PR alongside `SramInitConfig` ELF preload (closes #80).

## Deliverables

1. **`--cell-library <PATH>` CLI flag** on `jacquard sim`,
   `jacquard cosim`. Repeatable. Each path is parsed via
   `sverilogparse` at startup; results merged into a runtime
   `LeafPinProvider` extension.
2. **`<PATH>.cells.toml` autoload + `--cell-manifest <PATH>`
   override.** TOML schema as in ADR 0010 § Tier 2. Required
   field `schema_version = "1.0"`. Per-cell `kind` discriminator,
   v1.0 vocabulary.
3. **New code path in `aig.rs`**: after `PdkVariant::classify` falls
   through (no built-in match), consult manifest. For
   `kind = "ram"`, allocate `RAMBlock` in opaque mode — outputs
   routed to X-source slots, no port resolution.
4. **Tests**: TOML parsing unit tests; integration test exercising
   a synthetic `kind = "ram"` cell through AIG construction + sim
   (mini fixture, not the full tapeout design).
5. **Doc update — `docs/adding-a-pdk.md`**: new section "Adding
   third-party IP via manifest", linked from existing per-PDK
   recipes.

## Out of scope (deferred)

- Port-mapping schema (`[cells.NAME.ports]`). Future ADR.
- Other `kind` values beyond what the tapeout fixture exercises
  end-to-end (`ram`, plus `filler` if cheap parity demo). Adding
  other kinds is data-only and can land per-need.
- Migration of built-in `sky130.rs` / `gf180mcu.rs` classifiers to
  manifest data. Stays in this codebase as the fallback.
- `build.rs` pin-table scanner removal. Stays.

## Phasing

| Phase | Output |
|---|---|
| **P1** | `--cell-library` parsing + `LeafPinProvider` extension + tests. No AIG-construction changes yet — verify pin tables alone. |
| **P2** | Manifest TOML parser + `CellManifest` struct + `schema_version` validation. Standalone unit tests. |
| **P3** | `aig.rs` integration — manifest threaded through, new fallback path for `kind = "ram"` opaque mode. Add the `compute_x_sources`-style test exercising the new path. |
| **P4** | Smoke test against a representative reduced fixture; confirm `jacquard sim` clears `gf180mcu_ocd_ip_sram_*`. The full downstream-tapeout netlist is the real-world target but not in-tree. |
| **P5** | Doc update (`adding-a-pdk.md`); update `gf180mcu-enablement.md` § Follow-on cleanup to mark items 1/2/3 superseded by this work. |

Each phase is its own commit. No squashing until the spike feedback
loop confirms shape.

## Open questions to settle in code

- **Autoload path discovery**: spec says `foo.v` → `foo.cells.toml`
  sibling. Does that handle the multi-file library case (`a.v` +
  `b.v` sharing one manifest)? Probably yes — autoload each
  sibling, merge into the single `CellManifest`. Explicit
  `--cell-manifest` flag still wins for users who want a single
  consolidated file.
- **Conflict policy**: if a cell name appears both in a built-in
  classifier AND in a manifest, built-in wins (per ADR 0010
  integration ordering). Warn on conflict to surface accidental
  collisions.
- **Empty-library noise**: parsing a `.v` file containing only
  `(* blackbox *)` modules with no logic should succeed without
  warnings, since that's the expected shape for IP libraries.

## Not promised

- Memory contents simulation for `kind = "ram"` in v1.0. Documented
  in ADR 0010 § "kind = ram semantics in v1.0".
- Stable opaque-RAM port routing beyond "outputs are X-source
  slots". The set of outputs is what `sverilogparse` reports; if a
  cell's port list changes, the routing follows.
