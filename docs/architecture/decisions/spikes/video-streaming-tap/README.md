# C64 video streaming tap — spike renderer + runbook

Spike artifacts for the cosim signal-streaming tap (see
`../video-streaming-tap.md`). These are C64-specific and will move to the
c64-tapeout project once the tap lands; they live here for now so the spike is
runnable end-to-end.

- `c64_video_renderer.py` — a live renderer (pygame, `uv`-runnable) that reads
  the tap's UNIX-socket stream and displays the raster.
- `example-signal-streams.json` — the config fragment to merge into the C64
  `cocotb/sim_config.json`.

The feature itself lives in Jacquard on branch `feat/cosim-signal-stream-tap`
(`src/sim/cosim/signal_stream.rs`, config in `src/testbench.rs`, wiring in
`src/sim/cosim/mod.rs`). PR: gpu-eda/Jacquard#184.

## Automated unit tests (already green)

These cover the trigger/packing logic and the config serde contract:

```bash
cd /Users/roberttaylor/Code/Jacquard
cargo test --features metal --lib signal_stream
cargo test --features metal --lib testbench::tests::signal_stream
```

There is no automated end-to-end test yet — the raster render is validated
manually with the runbook below. That's the remaining spike-execution step.

## End-to-end runbook

Reference checkouts (same machine):
`/Users/roberttaylor/Code/Jacquard` and
`/Users/roberttaylor/Code/Retro/c64-tapeout`.

### 1. Build the feature branch of Jacquard

```bash
cd /Users/roberttaylor/Code/Jacquard          # branch: feat/cosim-signal-stream-tap
cargo build -r --features metal --bin jacquard
# → target/release/jacquard
```

### 2. Confirm the net names resolve (the known risk)

The video pad + strobe names come from the post-PnR netlist and must resolve
against it. Check before running cosim:

```bash
cd /Users/roberttaylor/Code/Jacquard
NL=/Users/roberttaylor/Code/Retro/c64-tapeout/final/pnl/chip_top.pnl.v   # or newest librelane/runs/*/…/chip_top.pnl.v
uv run netlist-graph search "$NL" "enablepixel"
uv run netlist-graph search "$NL" "bidir_CORE2PAD"
```

Expect `enablepixel` as `\i_chip_core.c64_u.enablepixel`. If the escaped
`\`-prefixed form doesn't resolve at cosim time, fall back to the plain RTL
name `enablePixel` in the config — the multi-candidate resolver accepts it.
This is the resolver-form open question from the spike.

### 3. Add the video tap to the C64 config

Merge the `signal_streams` array from `example-signal-streams.json` into
`/Users/roberttaylor/Code/Retro/c64-tapeout/cocotb/sim_config.json` as a
top-level key (alongside `clocks`, `qspi_memory`, …). Socket defaults to
`/tmp/c64-video.sock`.

### 4. Run cosim (binds the socket and listens)

Via the existing make target, pointed at the feature-branch binary:

```bash
cd /Users/roberttaylor/Code/Retro/c64-tapeout
make jacquard-cosim JACQUARD_BIN=/Users/roberttaylor/Code/Jacquard/target/release/jacquard
```

or the equivalent direct call:

```bash
/Users/roberttaylor/Code/Jacquard/target/release/jacquard cosim \
    --config cocotb/sim_config.json \
    final/pnl/chip_top.pnl.v \
    --top-module chip_top \
    --cell-library tools/jacquard_cell_lib/ocd_sram_shim.v \
    --max-clock-edges 5000000
```

Watch for `signal-stream 'video': listening on /tmp/c64-video.sock (6 signals)`.
`… did not resolve; tap disabled` means a name failed — go back to step 2.

### 5. Start the renderer (separate terminal)

```bash
cd /Users/roberttaylor/Code/Jacquard
uv run docs/architecture/decisions/spikes/video-streaming-tap/c64_video_renderer.py --socket /tmp/c64-video.sock
```

It waits for cosim, connects, and updates the window as samples arrive. First
frame takes a moment — a PAL frame is ~640k `clk32` edges, so
`--max-clock-edges 5000000` gives roughly 8 frames.

## Troubleshooting

- **Scrambled / permuted colours** → `colorIndex` bit order. Reverse the
  `\bidir_CORE2PAD[1..4]` entries in the config, or check the `out_drive[]`
  assignment in `chip_core.sv` for the true mapping. Framing (sync) is
  unaffected. See the open question below.
- **Window stays black** → cosim isn't reaching the video logic yet, or sync
  polarity is inverted. Sanity-check with `--trace-signals` on `hsync` to a VCD
  and confirm it toggles (sim-smoke saw `hsync=62`, so it should).
- **`tap disabled` in the log** → a net name didn't resolve; use step 2 to find
  the exact string, or try the plain `enablePixel` / bare pad names.
- **Renderer prints "cosim disconnected" repeatedly** → backpressure drops under
  a slow display; harmless, it reconnects. Lower `--max-clock-edges` for a
  gentler run.

## Net names (verified against the post-PnR netlist)

Escaped hierarchical names that survive LibreLane synthesis:

| Role | Net |
|------|-----|
| Pixel strobe (trigger) | `\i_chip_core.c64_u.enablepixel` |
| colorIndex[3:0] | `\bidir_CORE2PAD[1..4]` |
| hsync | `\bidir_CORE2PAD[5]` |
| vsync | `\bidir_CORE2PAD[6]` |

Confirm each resolves with `netlist-graph search <netlist> <name>` before a run.

## Open question: colorIndex bit order

The renderer assumes `signals` order `colorIndex[3], colorIndex[2],
colorIndex[1], colorIndex[0], hsync, vsync`, and the config maps those to
`\bidir_CORE2PAD[4,3,2,1,5,6]` in that order. If the picture's colours look
permuted, `\bidir_CORE2PAD[1..4]` maps to colorIndex bits the other way —
check the `out_drive` assignment in `chip_core.sv` and reverse the four
`colorIndex` entries in the config (or adjust `decode_sample` in the renderer).
Wrong order only permutes the palette; it doesn't break framing.
