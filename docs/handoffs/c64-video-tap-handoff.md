# Handoff — C64 cosim: video-tap e2e (#184)

**Issues:** gpu-eda/Jacquard#184. (#185 and #186, the two blockers this handoff
used to track, are fixed and in review.)

## Goal & next-up

**Goal:** get the C64 (`chip_top`) post-PnR netlist through `jacquard cosim` so
the video-streaming tap (#184) can render the VIC output.

Both Jacquard bugs that blocked the netlist from loading are fixed. **Next
session: run the C64 through cosim and do the #184 video-tap e2e.** That has not
been attempted yet — the two fixes are validated against Jacquard's own fixtures,
not against the C64 itself, so expect the C64 to surface its own issues.

## What landed

- **#185 derived (÷N) clock — PR #193**, branch `feat/derived-clock-divider`
  (rebased on main). A `clocks[]` entry names an internal net instead of a GPIO
  (`{ net, derived_from, divide_by }`); the AIG cuts the divider flop out of
  downstream clock cones and the scheduler drives the net as a commensurable ÷N
  domain. 14/14 goldens on CpuBackend and Metal. Fixture: `tests/derived_clock/`.
- **#186 SRAM offset panic — PR #194**, branch `feat/sram-macro-offset`, stacked
  on #193. **The issue's diagnosis was wrong**: this is not about
  cell-library-declared SRAM macros. It's staging. `flatten` read a staged
  endpoint index against the original AIG's endpoint accounting, which only
  coincide when staging is the identity. Any design with SRAMs that level-splits
  hits it — `multi_mem` (AIGPDK-native SRAM) panics identically under
  `--level-split 10`. The C64 triggers it for being big enough to split.
- **#183 QSPI shared-bus CS-gate** — merged (`346b0169`). Shared SCK/SIO +
  distinct CS up to `MAX_QSPI_MEMS = 4`. Bind flash pins to top-level `bidir_PAD`
  ports; the `bi_24t` split yields the `CORE2PAD`/`PAD2CORE` core-boundary
  signals, so no internal-net binding is needed.

## Next step — #184 video-tap e2e

Branch `feat/cosim-signal-stream-tap` (rebased on main), draft. Feature plus the
C64 renderer/config live in `docs/spikes/video-streaming-tap/`. Run cosim with
the `signal_streams` block
(`docs/spikes/video-streaming-tap/example-signal-streams.json`) and the pygame
renderer; runbook is in the video-tap README.

Verified net names: strobe `\i_chip_core.c64_u.enablepixel`, video pads
`\bidir_CORE2PAD[1..6]`.

Two open questions the e2e should answer: the net-name resolver form (does it
want the leading `\`?) and `colorIndex` bit order.

The C64's `clk32` is the `clk_pad ÷ 2` divider #185 addresses, so its config needs
a derived-clock declaration; see `tests/derived_clock/sim_config.json` for the
shape.

## Loose ends

- **Output VCD time axis is ~2x the stimulus VCD's** for the same run, found
  while validating #185 and **not caused by it**. `multi_mem` ends at 780000 in
  the stimulus and 1540000 in the output across the same 40 edges; the stimulus
  axis is the correct one. Every fixture and golden encodes it, so fixing it
  invalidates every committed golden. Filed as #195. It cost real time
  during #185 validation by making a correct ÷2 clock read as ÷4.
- Cross-session coordination is via the GitHub issues (both instances read them),
  not handoffs. Per-project git-identity plan (deferred):
  `~/Code/Claude/claude-git-identity-plan.md`.
