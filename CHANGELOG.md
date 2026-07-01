# Changelog

All notable changes to Jacquard are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the project follows [Semantic Versioning](https://semver.org/) from
`v0.1.0` onward (pre-1.0 caveat: minor bumps may carry breaking changes;
the public contracts in `docs/release-process.md` follow stricter rules).

## [Unreleased]

### Added

- **`JACQUARD_VENDOR_DIR`** env var to override the vendored-PDK root used
  for cell-library decomposition (`gf180mcu_fd_sc_mcu7t5v0` /
  `sky130_fd_sc_hd` functional models). Previously the path was hardcoded
  to `vendor/` relative to the process CWD, so a consumer running
  `jacquard` from another working directory had to symlink `vendor/` into
  place. Now it defaults to `vendor/` (unchanged behaviour) and honours
  `JACQUARD_VENDOR_DIR` when set. Follows the existing `JACQUARD_*`
  runtime-knob convention.
- **TNS / THS timing metrics** in `--timing-summary` and `--timing-report`.
  Alongside the existing worst-single-slack WNS/WHS, the report now carries
  Total Negative Slack and Total Hold Slack — the sum of every negative
  setup / hold slack across the run (`stats.total_setup_slack_ps` /
  `stats.total_hold_slack_ps`). Additive JSON-schema change, bumped to
  `1.2.0` (older reports still parse via `#[serde(default)]`). Salvaged from
  the timing logic in #17. (#9)

## [0.2.4] - 2026-06-26

Documentation, tooling, and submodule-hygiene release — no change to the
simulation binary's behaviour.

### Added

- **`scripts/check_doc_links.py`** plus a CI step that validates documentation
  links against the *rendered* mdBook page set, failing on links to pages
  absent from `SUMMARY.md` (which 404 on the published site). Replaces the
  previous file-existence-only check that missed this class of breakage.

### Changed

- Migrated the `eda-infra-rs` and benchmark `dataset` submodules from the
  `ChipFlow` org to `gpu-eda` forks. `eda-infra-rs` was rebased onto its
  upstream (`gzz2000/eda-infra-rs`), picking up the upstream license-string
  fix; the integration patches (Metal + HIP backends, ANSI-port and unary-NOT
  parser support) now ride a `jacquard-integration` branch. The parser fixes
  were also submitted upstream.

### Documentation

- Fixed published-site 404s by adding orphaned pages to the mdBook
  `SUMMARY.md` (project scope, timing correctness/validation, selective
  X-propagation, X-debugging, release process, cosim backend-portability and
  its phase plans, the JTAG debug server, netlist-graph xroots).
- Corrected stale "cosim is Metal-only" claims — cosim is now backend-portable
  (Metal/CUDA/HIP plus a CPU fallback) — and refreshed the Post-Phase-0
  roadmap status line.
- Removed the notional `GF130` commercial-PDK codename across the docs.
- Refreshed the docs index (cosim Quick Reference; Known Issues now link to
  GitHub; pruned Future Documentation Needs) and pointed the README's doc
  links at the published mdBook.
- Fixed the ADR 0011 `#80`→`#103` multi-SRAM cross-reference; folded the
  shipped cosim / CUDA-HIP timing implementation-plan docs into an ADR 0017
  amendment; repointed in-book links to repo files (CHANGELOG, tests,
  packaging) at GitHub URLs.

## [0.2.3] - 2026-06-25

Documentation release — no functional change to the binary.

### Documentation

- Reworked the README getting-started flow: an inline
  `brew install gpu-eda/homebrew-tap/jacquard` one-liner, the timing-feature
  status table moved into the generated docs, and stale `ChipFlow` /
  `chipflow.github.io` references corrected to `gpu-eda`.
- **Clarified the latch limitation** across the book: a raw latch left in the
  logic is unsupported, but **clock gating** (the `CKLNQD` integrated
  clock-gating cell) and **latch-based register-file / memory** (mapped to RAM
  via the memory-synthesis step) are supported, and asynchronous set/reset on
  flip-flops is fine — "async reset" was never the restriction.
- Corrected stale timing-feature status: CUDA + HIP `sim` now route setup/hold
  violations (`--timing-report`, symbolic messages); multi-corner timing IR is
  available via `--timing-corner`.
- ADR convention: a *refinement* to a decision is now recorded as a dated
  in-place **Amendment** (original decision preserved below it), reserving
  superseding ADRs for full reversals. ADR 0014's "no latches" constraint is
  amended accordingly (clock gating + latch-based memory are supported).

### Changed

- CI: `validate-install` authenticates cargo-binstall's GitHub API calls so the
  binstall channel check no longer hits the unauthenticated rate limit.

## [0.2.2] - 2026-06-25

### Fixed

- **Prebuilt macOS install channels worked only with Homebrew LLVM present**
  (caught by the new staging-validation pipeline). Two defects in the v0.2.1
  artifact: (1) `cargo binstall` failed because the tarball omitted
  `timing_analysis`, a non-optional `jacquard`-package bin — it now ships in
  the tarball (and the Homebrew formula installs it); (2) the binary links
  Homebrew LLVM's `libc++`/`libomp`, so it failed to launch on machines
  without LLVM — the Homebrew formula now `depends_on "llvm"`, and the
  binstall/raw-tarball LLVM runtime prerequisite is documented in
  `docs/installation.md`.

### Added

- **Staging install-validation pipeline** for release candidates. Tagging a
  SemVer pre-release (`vX.Y.Z-rc.N`) now publishes a GitHub *prerelease*
  (never "Latest"), and a new **Validate install (staging)** workflow
  (`validate-install.yml`, `workflow_dispatch`) runs the documented end-user
  install commands against that prerelease asset — `cargo binstall` (compile
  fallback disabled, so a missing/misnamed asset fails hard) and
  `brew install` (the formula repointed at the prerelease tarball, installed
  from a throwaway local tap). A green run is the promotion gate. See
  `docs/release-process.md` § Staging validation.

### Changed

- **Unified the first-party Rust crate versions.** `jacquard`,
  `opensta-to-ir`, and `timing-ir` ship together in one release tarball
  and now carry a single shared version (the two helper crates move
  `0.1.0` → `0.2.1` to match `jacquard`; both are `publish = false`, so no
  external version contract changes). A new `scripts/bump_version.py` is
  the single source of truth — `bump_version.py <X.Y.Z>` sets all three,
  and `release.yml` runs `bump_version.py --check <tag>` as a verify-guard
  that aborts a release if the tag and crate versions disagree.
  `netlist-graph` continues to version independently.

## [0.2.1] - 2026-06-23

Distribution fixes — the first release whose `release.yml` produces an
attached, working Metal binary. v0.1.0 and v0.2.0 shipped with no binary
asset (the publish step invoked `gh`, which is not on PATH on the macOS
runner) and a `sim` kernel that loaded its Metal library from a build-tree
path, breaking relocated binaries.

### Added

- **User-acceptance smoke gate** (`scripts/ci/user_acceptance_smoke.sh`):
  a shared post-install verification (`--version`, `sim`, `cosim`
  apb_trace per `docs/installation.md` § Verify) that now gates the
  release-workflow publish step. The missing `sim` coverage is what let
  v0.2.0 ship a broken binary.
- **`user-acceptance.yml`** standalone `workflow_dispatch`: build → install
  to a clean directory → run the smoke script against the relocated binary.
- **TestPyPI publish job** in `publish-netlist-graph.yml` (`workflow_dispatch`,
  environment `testpypi`) to validate the OIDC trusted-publisher wiring
  before the real PyPI tag.

### Changed

- `docs/installation.md`: `cargo binstall jacquard` →
  `cargo binstall --git …`. `jacquard` is not on crates.io (the vendored
  `eda-infra-rs` fork has diverged from its declared versions), so the
  `--git` form is required.
- `crates/timing-ir/Cargo.toml` repository URL → `gpu-eda`.
- `docs/plans/distribution.md`: added Phase 7 (eda-infra-rs upstreaming).

### Fixed

- **Release publish now attaches the binary**: switched from `gh release
  create` to `softprops/action-gh-release` (token API, no `gh` dependency).
- **Relocatable `sim` Metal binary**: the `sim` Metal kernel is now embedded
  via `include_bytes!` instead of being read from a build-tree path, so the
  shipped binary works when installed outside the build tree.

## [0.2.0] - 2026-06-23

### Added

- **Interactive JTAG debug server (`--jtag-server`)** (#124). `jacquard cosim
  --jtag-server <PORT>` opens a live `remote_bitbang` JTAG socket alongside the
  running GPU co-simulation, so an external debugger (OpenOCD, then gdb on top)
  can attach and drive the design's RISC-V Debug Module — halt/resume/step,
  read/write GPRs/CSRs/PC/memory, and `load` firmware — in lock-step with each
  debug transaction. The interactive sibling of `--jtag-replay` (recorded
  streams). Adds `--jtag-reconnect` (re-accept a debugger without restarting
  cosim) and a `jacquard jtag-openocd-config` helper that emits the matching
  `openocd.cfg`. Host-side only (no kernel changes); the CI gate is a real
  OpenOCD attach. See `docs/jtag-debug.md`.
- **`reg_init` register value-injection for cosim** (#108). A new
  `reg_init` array in the testbench JSON deposits a definite value into
  chosen registers at tick 0 with `$deposit` semantics — the seed clears
  the power-up X-mask, then the design's own logic drives the register
  normally (NOT `force`, which would pin a CDC crossing register and write
  zeros across the handshake). Each entry is `{ "name", "value", "width" }`;
  a multi-bit register resolves `name[0]`..`name[width-1]` independently.
  This is the register sibling of `sram_init` and the fix path for
  X-poisoned unreset CDC launch registers (#102): depositing on the launch
  flops lets X-aware cosim of debug-loaded firmware proceed. Composes with
  the x-assert detection work (#106). Regression: `tests/xprop_cosim/`
  `reg-init` mode (cosim A/B — deposit clears the X that `xprop` mode
  requires to persist).

### Fixed

- **cosim on SRAM-less designs** no longer panics. A design with zero SRAM
  produced a nil `MTLBuffer` (`new_buffer(0)`) whose `.contents()` is null;
  the SRAM data buffer is now sized to `max(1)` word (matching the X-mask
  shadow buffer), so pure-logic designs run under `jacquard cosim`.
- **`bi_24t` bidirectional pad core read-back** (#96). The AIG modelled a
  bidir pad's core read as `Y = PAD` always, so a design that drives a pad
  and reads it back in the same path read the *external* PAD net instead of
  its own drive `A` — wrong in two-state (reads the external stim/0) and
  conservatively-X under `--xprop` (the external pad is an undriven input).
  `AIG::from_netlistdb` now builds the tristate read-back combinationally as
  `Y = OE ? A : external`: on `OE=1` the core reads its own drive `A`
  (X-exact — known whenever `A` is), on `OE=0` it reads the external net.
  `in_c`/`in_s` input pads are unchanged. See ADR 0016. Unit test:
  `aig::gf180mcu_chip_top_tests::bi_24t_models_tristate_readback`.
- **Empty-partition flatten panic** (#128). A partition with zero boomerang
  stages — which mt-kahypar can emit at high partition counts (finer
  partitioning or a large `--num-blocks`) — panicked in
  `FlattenedScriptV1::from` ("index out of bounds: len is 0"). Such partitions
  do no work and are now dropped before flattening.

## [0.1.0] - 2026-06-04

First numbered release. Metal (macOS/Apple Silicon) is the shipped
distribution target; CUDA and HIP remain source-build until their CI
runners land (ADR 0018).

### Added

- **X-source debugging: `jacquard xsources` + `netlist-graph xroots`**
  (#98). A two-command workflow to answer "why does signal S read X under
  `cosim --xprop`?" as a static query instead of a trace→guess→re-run loop.
  `jacquard xsources <netlist> --config c.json -o xsources.json` statically
  enumerates every X-source — unreset/reset DFF Q outputs, SRAM read ports,
  and (with `--config`) undriven primary inputs — as a schema-versioned JSON
  manifest, names resolved through netlistdb (no GPU required).
  `netlist-graph xroots <netlist> <signal> --xsources xsources.json` then
  reverse-reachability-walks the signal's cone (through DFF data pins,
  skipping clock/reset), intersects with the manifest, and reports the nearest
  X-source frontier classified by kind; `--emit-trace` writes a
  `--trace-signals` list for a confirming `--xprop` run. DFF/SRAM sources
  resolve by driving-cell instance, so jacquard's net-name canonicalisation
  doesn't break the round-trip. Full guide: `docs/x-debugging.md`. The cosim
  GPIO-index parser (`parse_gpio_index`) is now shared with the `xsources`
  driven-set computation so the two stay consistent.

- **Multi-corner support** (WS2.4, commits `5822343` / `530bb36` /
  `59fde04`). `opensta-to-ir` accepts `--liberty NAME=PATH` to attach
  Liberty files to named PVT corners (bare paths still go to a
  "default" corner). The Tcl driver runs `define_corners` + per-scene
  `read_liberty -corner`, emits one record per (key, corner), and the
  Rust builder dedupes them into IR records carrying
  multi-`TimingValue` vectors. Consumer side: new `--timing-corner
  <NAME>` flag on `jacquard sim` selects the corner index from
  `ir.corners()` at load time; defaults to corner 0 when unset. Single-
  corner data continues to work unchanged.
- **`--timing-report <path.json>`** end-of-run structured timing report
  (commit `58a7a04`). Schema-versioned (`schema_version = "1.0.0"`,
  additive-only per ADR 0008's stability contract). Contains per-cycle
  violation list, per-word aggregates, top-10 worst-slack rankings per
  kind, run metadata, and aggregate stats. Sample fixture at
  `tests/timing_ir/sample_reports/two_violations.json`. Schema and
  consumer guide live in `src/timing_report.rs` and
  `docs/timing-violations.md`.
- **`--timing-summary`** human-readable text summary on stdout
  (commit `44e70a0`). Independent of `--timing-report`; either or both
  can be set, the report is built once and routed to the requested
  outputs.
- **Symbolic violation messages**: setup/hold violation lines now name
  the offending DFFs (`top/cpu/regs[7][bit 22] [word=42]`) instead of
  bare state-word indices (commit `0432d9a`). The `[word=N]` suffix is
  preserved for grep/tooling. New `WordSymbolMap` in `src/flatten.rs`
  built once at sim startup.
- **OpenSTA detection + version check** (WS-RH.1, commit `c9c393b`).
  When `--sdf` is requested but OpenSTA is missing, too old, or fails
  to probe, Jacquard now hard-fails with a remediation message instead
  of silently dropping timing data. Newer-than-tested versions warn but
  proceed. Tested against OpenSTA `3.1.0`
  (vendored at `vendor/opensta/`).
- **`--timing-report` violations cap**: per-cycle `violations` array is
  capped at 100k records by default (~8 MB JSON). Override with
  `--timing-report-max-violations <N>`; `0` disables the cap.
  `stats.violations_truncated` records overflow. Setup/hold totals
  and worst-slack rankings always reflect every observed event.
- **Pillar B Stages 1+2** (commits `c403cc8`, `6767c3e`): `ClockArrival`
  IR records emitted by `opensta-to-ir`; per-DFF capture-side clock
  arrival folded into setup/hold checks with `DFFConstraint::clock_arrival_ps`.
  Closes the main per-flop skew accuracy lever ahead of Phase 2.
- **`opensta-to-ir` golden-IR regression corpus** (WS4, commits
  `90558bb` / `6997096` / `9e25bc2`): seed entry `aigpdk_dff_chain` covers
  all four IR record types; runner + regen helper + CI hookup in place.
- **GF180MCU power-pin + wired-filler shortcuts** in
  `GF180MCULeafPins::direction_of`. Phase 6 left two gaps that only
  surfaced on real post-P&R chip-top netlists (`gf180mcu-project-
  template` / wafer.space LibreLane flow): (1) Verilog cell models
  declare power as `inout VDD, VSS;`, but `build.rs` only parses
  `input`/`output` — so the generated pin table lacked VDD/VSS
  entries, panicking on the first `.VDD(...)` wired in
  `antenna`/`endcap`/`fillcap`/etc.; (2) `cor`/`fill*`/`dvdd`/`dvss`
  etc. were assumed to instantiate with empty port lists, but real
  netlists wire all four power pins on every filler instance. New
  short-circuit ahead of the pin-table lookup: any pin named
  `VDD`/`VSS`/`VNW`/`VPW`/`VPB`/`VNB`/`VPWR`/`VGND`/`DVDD`/`DVSS`/
  `AVDD`/`AVSS` resolves to `Direction::I` (constant external
  driver — the enum has no `Inout`), and any pin on a known filler
  cell falls through to the same. Three new tests in
  `gf180mcu::tests` cover the power-pin shortcut, the wired-filler
  shortcut, and a realistic chip-top fixture matching the
  `gf180mcu-project-template` netlist shape. Advances Phase 7 of
  the GF180MCU enablement plan from "synthetic fixtures only" to
  "real wafer.space post-P&R netlists parse end-to-end".
- **Config-driven AHB/APB bus transaction tracing** for `cosim`
  (ADR 0013). Declare buses in `sim_config.json` (`bus_traces`) and write
  decoded transactions to CSV via `--bus-trace-csv`. APB3 is supported
  (GPU captures raw beats, the CPU runs the protocol FSM); AHB-Lite /
  AHB5 are planned. Validated by `tests/apb_trace/`. See
  `docs/bus-tracing.md`.
- **Prebuilt-binary distribution groundwork** (ADR 0018): the Metal
  binary now embeds its kernel library so a downloaded binary is
  relocatable, and `cargo binstall` metadata is in place. GitHub
  Releases + a Homebrew tap (macOS/Metal) follow.
- **`netlist-graph drivers --data-only`** (#100): on sequential cells,
  follow the data (`D`) pin only and skip clock/set/reset/enable pins,
  so tracing a register stays on the data path instead of diving into
  the clock and reset distribution trees.
- **Selective X-propagation** (`--xprop`, #95) for both `sim` and
  `cosim` (ADR 0016). Uninitialised state — power-up DFFs, undriven
  primary input pads, and uninitialised SRAM — propagates as `x` through
  the design and into the output VCD instead of silently reading as `0`.
  X is seeded only at genuine X-sources: a prior seed-template bug
  cleared the X-mask over DFF-Q feedback reads, so X never originated and
  any sequential design ran two-state. Undriven primary inputs read X in
  cosim; SRAM writes and preloads clear the per-cell X-mask shadow.
  End-to-end CI guards live in `tests/xprop_cosim/`. See ADR 0016 and
  `docs/plans/cosim-xprop.md`.

### Changed

- **ADR 0006 amended 2026-05-02** (commit `a4b1c81`): subprocess
  invocation of user-installed OpenSTA from the shipped runtime is now
  permitted, provided OpenSTA is not bundled and not linked into the
  Jacquard binary. Phase 3 (native Rust SDF→IR) is no longer release-
  gating. Background: GPL-3 § 5 ("aggregate") and FSF subprocess/IPC
  doctrine.
- **`process_events` API**: now takes a `ReportingCtx<'_>` bundle
  (`word_resolver` + `violation_observer`) instead of two separate
  optional callbacks. Default callers pass `ReportingCtx::default()`.
- **Hand-rolled SDF parser deleted** (WS3 phase 3.4, commit `9b2eb00`):
  `--sdf` now subprocesses `opensta-to-ir`, which produces timing IR
  consumed by `load_timing_from_ir`. The IR path (`--timing-ir`) is the
  canonical input.
- **CLI flag rename (pre-release, no compatibility alias):** cosim
  `--timing-vcd <PATH>` → `--output-vcd <PATH>` (the output VCD —
  functional by default, timed when `--timing-ir`/SDF is supplied), and
  sim `--timing-vcd` → `--timed`. Fixes a misnomer that steered pre-PnR
  functional users away from the one flag they needed.

### Fixed

- `cosim` warns when the positional netlist argument differs from the
  config's `netlist_path` (the positional wins; logs no longer silently
  describe a different netlist than the one running).
- Progress logging no longer floods single-tick runs — it now fires once
  per ~100k-edge window regardless of batch mode.
- Tracing docs corrected: traced nets land in `--output-vcd` only (not
  `--stimulus-vcd`), and the stale "forces single-tick mode" help text
  removed (VCD capture uses a GPU ring buffer, preserving batched
  dispatch).
- `netlist-graph cone` no longer stops at driven internal nets and
  mislabels them `[primary input]` (#99). Inverting/tie output pins
  (`ZN`, `LO`, `HI`, `QN`, `CO`, …) were misclassified as cell *inputs*,
  so their output nets had no driver edge and looked undriven. The cone
  now follows them, and a genuinely undriven internal net is flagged
  `[undriven — X-source]` distinctly from a real top-level port; constant
  tie-cell outputs (sky130 `conb_1`) are labeled `[constant: …]`.
- `netlist-graph` register detection (`_is_register`) now recognises
  reset/set/scan flop variants (sky130 `dfrtp`/`dfstp`/`dfbbn`/`sdf*`,
  gf180 `dffrnq`/`latrnq`), not just the plain DFF, by matching the
  functional cell name after the library prefix.

### Deprecated / Removed

- ADR 0003 (OpenTimer as primary STA) **superseded** 2026-05-01
  (commit `d002bde`). OpenSTA-out-of-process is the sole STA path.
- `src/sdf_parser.rs` removed (WS3 phase 3.4); replaced by `opensta-to-ir`.

### Documentation

- New: `docs/release-process.md` — lightweight release procedure.
- Updated: `docs/why-jacquard.md` — output-interface section now
  describes shipped surface; deferred items moved to "Still on the
  wishlist".
- Updated: `docs/usage.md` — § Timing-Aware Simulation describes
  `--timing-ir` vs `--sdf` paths plus OpenSTA version requirements.
- Updated: `docs/timing-violations.md` — new § Text Summary and
  § Structured JSON Report.
- Plan files: `docs/plans/post-phase-0-roadmap.md` tracks current
  workstreams; Phase 0 / Phase 1 closed.
- New: `docs/bus-tracing.md` and `docs/signal-tracing.md` — user guides
  for the two cosim observability features (decoded transactions vs. raw
  nets). ADR 0018 + `docs/plans/distribution.md` cover the install model.

### Known limitations

- `--timing-report` and `--timing-summary` are wired only into the
  Metal sim path. The CUDA, HIP, and cosim paths detect violations on
  the GPU but do not yet route them through `process_events`. Tracked
  as a follow-up.
- The per-cycle `violations` array in `--timing-report` is capped at
  100k records by default (override via `--timing-report-max-violations`).
  Setup/hold totals + worst-slack always reflect every observed event;
  only the per-cycle list is bounded.
- `worst_slack` ranking is populated only from observed violation
  events. Closest-to-violation tracking on a run that *passed* timing
  needs GPU-side near-miss instrumentation; deferred.
- Bidirectional pads under `--xprop` resolve to conservative X when the
  output enable is deasserted; the precise tristate-mux read
  (`Y = OE ? A : external`) is deferred (#96).
- Timed output (sim `--timed` / cosim `--output-vcd` with timing data)
  routes violations through `process_events` only on the Metal sim path.
  CUDA / HIP detect violations on the GPU but don't currently route
  them; the JSON / text outputs only fire on Metal today.

[Unreleased]: https://github.com/gpu-eda/Jacquard/compare/v0.2.4...HEAD
[0.2.4]: https://github.com/gpu-eda/Jacquard/compare/v0.2.3...v0.2.4
[0.2.3]: https://github.com/gpu-eda/Jacquard/compare/v0.2.2...v0.2.3
[0.2.2]: https://github.com/gpu-eda/Jacquard/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/gpu-eda/Jacquard/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/gpu-eda/Jacquard/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/gpu-eda/Jacquard/releases/tag/v0.1.0
