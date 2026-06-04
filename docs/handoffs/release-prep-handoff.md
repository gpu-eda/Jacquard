# Handoff — release prep / open threads

**Created:** 2026-06-04
**Branch:** main (`296cc12`)
**Working tree:** clean

## Goal & next-up

**Goal:** carry the open release/distribution threads now that #95
(selective X-propagation in cosim) is **done and merged**. This handoff
swaps back in for the `cosim-xprop` one (which is retired — its content is
folded into ADR-0016 and `docs/plans/cosim-xprop.md`).

**Next-up (pick one):** cut **`v0.1.0`** (maintainer-provisioned
distribution, ADR 0018) — the highest-value remaining item — or knock out
the `vendor/eda-infra-rs` bump if upstream has landed the license fix.

## Just done (#95 X-propagation — MERGED `296cc12`)

`--xprop` now works end-to-end in **both `sim` and `cosim`**, which it
never actually did before. Root cause was a seed-template bug: the
power-up X-mask cleared *all* `input_map` positions, but those include
DFF-Q feedback reads — so uninitialised DFFs read as known `0` and X never
originated (silently two-state for any sequential design). PR #97, 7
commits:

- seed X at genuine X-sources only (`vcd_io::xprop_xmask_template`)
- end-to-end CI guards (`tests/xprop_cosim/`, `check.py` 3 modes; sim +
  cosim, fatal — the missing guard that let the bug ship)
- Phase 3 — undriven primary inputs read X in cosim (`compute_x_capable_pins(treat_inputs_as_x_sources)`,
  `xprop_xmask_template_cosim`, `state_prep`/`gpu_apply_flash_din` clear
  driven-bit X-mask)
- Phase 4 — observe-kernel offset guard (already correct by construction;
  APB3 + dual-UART re-run under `--xprop`, identical decode)
- test outputs → gitignored `target/test-out/` (+ shared `ensure_parent_dir`)
- SRAM **preload** X-mask fix (`apply_chunks` now clears the shadow for
  preloaded cells)

Details: ADR-0016 amendments, `docs/plans/cosim-xprop.md`.

## test-tapeout-1 SRAM-X (resolved — NOT a Jacquard bug)

`feat/dual-core` SINGLE design reported "SRAM returns X always" with an
uncommitted bump to the #97 branch. **Verified Jacquard's runtime SRAM
write correctly clears the X-mask** (minimal explicit-port RAM repro: a
known write during reset reads back known under `--xprop`; repro at
`/tmp/claude/sram_repro/` — `top_rw.gv` + `tinyram.{v,cells.toml}`). So
their X is `--xprop` **correctly surfacing uninitialised state** in the
SRAM write/control path. **Recommendation for that repo:**
`--trace-signals` the SRAM `D`/WE nets to find where X enters, and reset
that logic. (Their preload is disabled, so the preload fix above isn't
their issue.)

## Open threads (carried; re-verify before acting)

- **NVIDIA runner won't POST** — hardware (missing CPU EPS power cable,
  cable on order). Once up: distribution **Phase 4** (CUDA/HIP release
  rows in `release.yml`) + re-enable CUDA CI (`ci.yml` — CUDA/HIP jobs
  currently `skipping`) + CUDA/HIP timing-report routing
  (`process_events`/`ReportingCtx`).
- **`vendor/eda-infra-rs` bump** — blocked on upstream sverilogparse
  license-string fix; `git -C vendor/eda-infra-rs fetch && git show
  origin/master:sverilogparse/Cargo.toml | grep license`.
- **Maintainer-provisioned distribution** (ADR 0018): cut `v0.1.0` (→
  draft release + tap formula PR), create the `netlist-graph` PyPI
  project + trusted publisher. See `packaging/README.md` and
  `docs/release-process.md`.

## Deferred #95 follow-ups (not blocking)

- **#96** — bidir tristate-mux read `Y = OE ? A : external` (today bidir
  reads fall out as conservative-X via the undriven-input rule).
- **Multi-macro SRAM X-mask** — verified single-macro 1-bit write-clears-X;
  the 16-macro / 8-bit byte-mask path (test-tapeout-1's wrapper) wasn't
  directly exercised. Same per-cell logic, but worth an 8-bit repro if a
  multi-macro xprop issue surfaces.

## Process notes (learned this session)

- **`gh pr checks --watch` exit code is unreliable** — returned 0 even
  with a failed/superseded Metal job, and loses the branch on
  force-push/branch-switch. Trust `gh pr checks <pr>` / `gh run view`
  (terminal-state query) instead.
- **"Passes locally" can hide CWD/ordering assumptions** — the sim
  output-VCD writer's missing `mkdir` only failed in CI because the demo
  step ran *first* (fresh `target/test-out/`); locally a prior cosim run
  had created the dir.

## References

- ADR: `docs/adr/0016-selective-x-propagation.md` (amendments)
- Plan: `docs/plans/cosim-xprop.md` (all phases done)
- Merged PR: #97 → `main` `296cc12`
- Distribution: ADR 0018, `packaging/README.md`, `docs/release-process.md`
- Issues: #95 (done), #96 (bidir tristate mux)
