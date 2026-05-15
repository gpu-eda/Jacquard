# Plan — WS2: `opensta-to-ir`

**Status:** Implemented — historical record. All five phases (2.1–2.5)
plus Pillar B Stage 1 (per-DFF CLOCK_ARRIVAL records) and release
hardening (WS-RH.1 OpenSTA version probe) have shipped. The crate
lives at `crates/opensta-to-ir/`. Current scheduling for further
timing-model fidelity work is tracked in `post-phase-0-roadmap.md`.

**Phase:** 0 (executed WS2 from `phase-0-ir-and-oracle.md`).
**Predecessors:** WS1 (`crates/timing-ir`, schema and round-trip — done), ADRs 0001 / 0002 / 0005 / 0006.

## Goal

Deliver a production-quality preprocessing tool that consumes a design's timing inputs and emits a `timing-ir` file suitable for downstream Jacquard consumption. End-to-end:

```
.lib + .v + .sdf + .spef + .sdc  →  opensta-to-ir  →  design.jtir (+ design.json)
```

`opensta-to-ir` is shipped as a release artefact (per ADR 0006) and is also used by Phase 0 WS3's interim `jacquard sim --timing-sdf` runtime hook.

## High-level architecture

Three components, single binary:

```
┌─────────────────────────┐     ┌─────────────────────────┐     ┌─────────────────────────┐
│  Rust CLI / driver      │     │  Tcl dump script        │     │  Rust IR builder        │
│  (clap, subprocess mgmt)│ →   │  (runs in OpenSTA proc) │ →   │  (parses dump, builds   │
│  Validates inputs       │     │  Emits canonical dump   │     │   FlatBuffers IR)       │
└─────────────────────────┘     └─────────────────────────┘     └─────────────────────────┘
            │                                                                   │
            └──────────────────── one process invocation ───────────────────────┘
```

The Rust CLI invokes OpenSTA as a subprocess, writes the Tcl driver script to a temp directory, runs `sta -f $tmpdir/dump.tcl`, captures the dump file, and converts to IR. The Tcl driver lives at `crates/opensta-to-ir/tcl/dump_timing.tcl` and is embedded in the binary via `include_str!()` so the binary is self-contained at runtime — no separate Tcl file needs to ship alongside it.

OpenSTA is located via `scripts/build-opensta.sh --print-binary` first (the canonical install path for the vendored submodule), then falling back to a `PATH` lookup, then `--opensta-bin <PATH>` override.

Reasons for this shape:

- OpenSTA's structured Tcl API (`get_timing_edges`, `get_timing_arcs_from`, etc.) gives access to OpenSTA's internalised timing graph directly. Walking it is simpler than parsing OpenSTA's SDF output back through a second-generation parser.
- The Tcl script is the only OpenSTA-specific code; the Rust side is format-only and can later be reused with other producers (Phase 3 native Rust parser, future OpenTimer adapter).
- Subprocess invocation preserves Jacquard's permissive license posture (ADR 0001).

## Tcl dump format

A simple line-oriented record format. Each line is one annotation. Fields are tab-separated. Strings with tabs/newlines are quoted with simple `"..."` and `\t`/`\n` escaping. Header / footer lines mark the document.

```
# format-version: 1
# generator-tool: opensta-to-ir 0.1.0
# generator-opensta: <opensta version string>
# input-files: <comma-separated list>
CORNER	<index>	<name>	<process>	<voltage>	<temperature>
ARC	<cell_instance>	<driver_pin>	<load_pin>	<corner_index>	<rise_min>	<rise_typ>	<rise_max>	<fall_min>	<fall_typ>	<fall_max>	<condition>	<origin>
INTERCONNECT	<net>	<from_pin>	<to_pin>	<corner_index>	<min>	<typ>	<max>	<origin>
SETUP_HOLD	<cell_instance>	<d_pin>	<clk_pin>	<edge>	<corner_index>	<setup_min>	<setup_typ>	<setup_max>	<hold_min>	<hold_typ>	<hold_max>	<condition>	<origin>
VENDOR_EXT	<source>	<source_tool>	<kind>	<base64_payload>
# end
```

Why line-oriented (not JSON): Tcl emits this trivially with `puts`. Rust parses it with a `BufReader` line-at-a-time, no streaming-JSON parser. Mismatched lines fail loud at the unit level, not after parsing 100MB of nested JSON.

The format is a private interface between the bundled Tcl script and the bundled Rust binary — both ship together in one release artefact. We reserve the right to change the format any time as long as both sides update.

## Rust binary

```
opensta-to-ir [OPTIONS] --output <PATH>

Inputs (at least one liberty + one verilog required):
  --liberty <PATH>...           One or more Liberty files (-r overlay supported by OpenSTA).
  --verilog <PATH>...           One or more Verilog netlists.
  --sdf <PATH>                  Optional. Back-annotated delays.
  --spef <PATH>                 Optional. Parasitics; required for SPEF-based delay calc.
  --sdc <PATH>                  Optional. Constraints (clocks, input delays).
  --top <NAME>                  Top-level module name. Required.
  --corner <NAME>...            Corner name(s). Default: "default".

Output:
  --output <PATH>               IR binary output path (.jtir).
  --json <PATH>                 Optional. JSON sidecar via flatc round-trip.

Behaviour:
  --opensta-bin <PATH>          Override the OpenSTA executable path. Default: probe via
                                `scripts/build-opensta.sh --print-binary`, then fall back to PATH.
  --keep-tmp                    Keep the Tcl script and dump file in $TMPDIR for debugging.
  --min-arcs <N>                Fail if fewer than N timing arcs are emitted. Default: 1.
  --allow-empty-parse           Disable the --min-arcs check. For test fixtures only.
  --strict-tcl                  Treat OpenSTA Tcl warnings as errors.
  -v, --verbose                 Echo OpenSTA's stderr to ours. Default: capture and replay only on failure.

Exit codes:
  0    IR produced successfully.
  1    OpenSTA returned an error.
  2    Tcl dump format error or IR-build failure.
  3    Parser-success assertion failed (--min-arcs not met).
  4    Argument validation error.
```

Internal flow:

1. Validate args (required files exist, top name non-empty).
2. Locate OpenSTA binary; verify version is in supported range.
3. Render Tcl driver script into `$TMPDIR` (or stdin).
4. Spawn `opensta -f <script>`; capture stdout/stderr/exit.
5. Read dump file from `$TMPDIR/<uniqued>.osd` (OpenSTA dump).
6. Parse dump, build IR via `timing-ir` crate's FlatBuffers builders.
7. Apply `--min-arcs` assertion (see WS5 portion below).
8. Write `.jtir` (and `.json` if requested).
9. Surface any captured warnings on stderr.

## Multi-corner handling

OpenSTA's `define_corners` and `set_scene` commands drive multi-corner analysis. Our flow:

- Caller passes `--corner ss_125C_1v08 --corner tt_25C_1v80 --corner ff_-40C_1v98`.
- Tcl script calls `define_corners` once with the union, then iterates `foreach corner [get_corners] { ... }` and emits `CORNER` + `ARC`/`INTERCONNECT`/`SETUP_HOLD` lines tagged with the corner index.
- Single-corner designs use one entry — same code path, no special case.

PVT extraction (process / voltage / temperature) — OpenSTA exposes these via Liberty's operating conditions. Tcl extracts via the corner's pvt object. If unavailable, `process="?"`, `voltage=0.0`, `temperature=0.0`.

## Vendor extensions

OpenSTA does not expose a single mechanism for arbitrary annotations. For Phase 0 WS2:

- We do not produce `VENDOR_EXT` records.
- The IR's vendor-extension passthrough remains a forward-looking feature; a future producer (a commercial-tool-aware adapter) will populate it.

Tcl-side parsing of vendor-specific Liberty `simulation` blocks or SDF `(VENDOR …)` constructs is **not** in scope for Phase 0 WS2.

## Parser-success assertion (WS5 portion)

Per `phase-0-ir-and-oracle.md` WS5: "Assertions in `opensta-to-ir`: non-zero IOPATHs / timing arcs resolved on non-trivial SDF input. Exit non-zero with a clear diagnostic when below threshold."

Implementation:

- `--min-arcs <N>` flag with default `1`.
- After IR is built, count `TimingArc` records in the buffer.
- If below threshold and `--allow-empty-parse` was not passed, exit code 3 with message: `opensta-to-ir: produced N timing arcs (--min-arcs <M>); use --allow-empty-parse for empty-fixture tests`.
- Liberty parser-success assertion already lives in Jacquard's `TimingLibrary::parse` (see commit `5db131e`) — `opensta-to-ir` invokes OpenSTA's own Liberty reader rather than Jacquard's, so it surfaces missing-cell issues via OpenSTA's exit status (not our concern at this layer).

## Test plan

### Fixture progression — minimum-viable to representative

1. **inv_chain_pnr** (already in `tests/timing_test/`): smallest design with real SKY130 cells and SDF. Verify single arc per inverter, correct rise/fall, single corner.
2. **MCU SoC subset**: representative of the real Jacquard flow. Verify the count of arcs matches a known baseline; spot-check a handful of arrival times against `report_timing` output.
3. **Multi-corner synthetic**: hand-built tiny design with `ss/tt/ff` Liberty corners, verify the IR carries 3 corner records and 3 sets of values per arc.

### Test types

- **Unit tests (Rust)**: dump-format parser tested against synthetic dump strings (no OpenSTA needed).
- **Integration tests (Rust + OpenSTA)**: invoke the binary against committed fixtures, diff the resulting IR against golden IR via `timing-ir-diff`. Each integration test gates itself on `scripts/build-opensta.sh --print-binary` succeeding — when the OpenSTA binary is unbuilt, tests skip with a clear "run scripts/build-opensta.sh" message rather than failing. CI runs them after building OpenSTA via the script.
- **Failure-mode tests**: missing OpenSTA, malformed Tcl dump, zero-arc input, missing required argument — each surfaces the expected exit code.

### CI integration (closes WS4 remaining work)

- A new CI job runs `opensta-to-ir` on each `tests/timing_ir/corpus/<name>/inputs/` and diffs against `expected.jtir` via `timing-ir-diff`. Fails loud on diff or exit-code regression.
- Stress-corpus run is deferred to Phase 1.

## Phased implementation

Splitting WS2 into focused PRs keeps reviewability tight. Each phase exits with a runnable end-to-end on its scope:

| Phase | Scope | Exit signal | Status |
|---|---|---|---|
| 2.1 | Single-corner, timing-arc IOPATHs only. CLI scaffolding. | AIGPDK AND2 round-trip clean through `opensta-to-ir` end-to-end. | ✅ Shipped (`dc3db4a` scaffold + `3997e06` subprocess plumbing + `50b8600` real Tcl extraction). |
| 2.2 | Add interconnect delays (`wire`-role edges, with optional SPEF). | Multi-cell design produces INTERCONNECT records that round-trip. | ✅ Shipped (`67210c0`). Test: `chain_with_sdf_emits_interconnect_delay`. |
| 2.3 | Add setup/hold checks. | DFF setup/hold round-trips end-to-end. | ✅ Shipped (`8343b14`). Test: `aigpdk_dff_emits_setup_hold_records`. Recovery / removal / width checks remain out of scope. |
| 2.4 | Multi-corner. | 3-corner synthetic fixture produces 3-corner IR. | ✅ Shipped (`530bb36` builder + `59fde04` per-corner Tcl emission + `d110174` integration test + `50f4bf5` real-sky130 multi-corner follow-up). Tests: `aigpdk_dff_emits_per_corner_timing_values`, `sky130_multi_corner_emits_per_corner_values`. |
| 2.5 | CI corpus integration; golden-IR fixtures for representative designs. | WS4 corpus job in CI; WS2 task complete. | ✅ Shipped (`90558bb`). Runner: `cargo test -p opensta-to-ir corpus`. |

**Beyond original WS2 scope:**

- **Pillar B Stage 1** — per-DFF `CLOCK_ARRIVAL` records (`c403cc8`). Adds clock arrival times to the IR so downstream consumers can compute per-DFF setup/hold margins without re-running OpenSTA. Test: `dff_with_sdc_clock_emits_clock_arrival`. Tracked separately in `post-phase-0-roadmap.md`.
- **Release hardening WS-RH.1** — hard-fail on missing or too-old OpenSTA, with version probe and usage diagnostics (`c9c393b`). Tests: `locate_accepts_min_tested_version`, `locate_flags_newer_than_tested`. Tracked in `post-phase-0-roadmap.md` § Release hardening.

WS3 (delete `src/sdf_parser.rs` + wire interim runtime hook) was unblocked once Phase 2.3 minimum landed and has also shipped — see `ws3-delete-sdf-parser.md`.

## Open questions — resolution

Resolutions from implementation:

- **OpenSTA version pinning** — **Resolved** by WS-RH.1 (`c9c393b`). Binary probes OpenSTA's `version_string`, accepts a `[MIN_TESTED, MAX_TESTED]` range, prints a usage diagnostic with the supported range on mismatch.
- **OpenSTA installation** — **Resolved**. `scripts/build-opensta.sh` ships with `--print-binary` for the dependency probe; integration tests skip cleanly when the binary isn't built. Documented in the script's `--help` and the post-Phase-0 roadmap.
- **Tcl-script versioning** — **Resolved**. `# format-version: 1` header check is enforced in `dump.rs`; the binary refuses unknown versions with an explicit error.
- **Conditional arcs (SDF `COND`)** — **Partial**. The `condition` field is plumbed end-to-end (dump format → Rust parser → IR builder), but the Tcl emission side does not yet populate it for conditional variants. Defer until a real design surfaces a `COND` arc that needs distinguishing.

Still open / deferred:

- **Long-running designs**: streaming dump emission (Tcl flushing line-by-line, Rust incremental read) — defer until profiling on a real SoC shows memory pressure.
- **Strict Tcl error handling**: `--strict-tcl` flag was specced but not implemented. Current behaviour captures all stderr and replays on failure; no warning-to-error upgrade path. Land if it becomes a real CI hygiene concern.

## Risks

- **OpenSTA's Tcl API is large and not all of it is documented**. Some primitives we'll need (e.g., per-corner delay values for a specific arc) may require digging through `Sta.cc`. Mitigation: budget time, lean on `report_path` text output as a fallback if the structured API proves opaque for a given query.
- **OpenSTA may be slow on big designs** — the structured walk over millions of arcs is single-threaded. Mitigation: `--keep-tmp` for profiling, accept slow phase-0 runs, optimise later if it blocks CI.
- **Format drift between Tcl and Rust** — both sides advance together; the `format-version` line plus version-mismatch fail-loud catches drift. Add a unit test that the Rust parser rejects an unexpected version line.

## Non-goals

- A general SDF parser. (The whole point: avoid that.)
- Wire-level reactivity or feedback to OpenSTA mid-run (this is a one-shot extract).
- Comparison against OpenTimer (that's a separate ADR-0003-spike concern).
- Replacing OpenSTA's role as oracle in CI — `opensta-to-ir` is a producer, not a checker.

## References

- `../adr/0001-opensta-as-oracle.md` — subprocess model, license posture.
- `../adr/0002-timing-ir.md` — IR contract this tool emits.
- `../adr/0005-opensta-vendoring-and-corpus.md` — `vendor/opensta/` submodule.
- `../adr/0006-sdf-preprocessing-model.md` — interim runtime hook + release-time cutover.
- `phase-0-ir-and-oracle.md` — WS2 row in the work breakdown.
- `crates/timing-ir/schemas/timing_ir.fbs` — schema this tool produces.
- `vendor/opensta/doc/StaApi.txt` — OpenSTA Tcl API reference.

---

**Last updated:** 2026-04-28 (design); 2026-05-15 (status flip to Implemented).
