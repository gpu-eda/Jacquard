# C64 video streaming tap — spike renderer

Spike artifacts for the cosim signal-streaming tap (see
`../video-streaming-tap.md`). These are C64-specific and will move to the
c64-tapeout project once the tap lands; they live here for now so the spike is
runnable end-to-end.

- `c64_video_renderer.py` — a live renderer (pygame, `uv`-runnable) that reads
  the tap's UNIX-socket stream and displays the raster.
- `example-signal-streams.json` — the config fragment to merge into the C64
  `cocotb/sim_config.json`.

## Run it

1. Add the `signal_streams` block from `example-signal-streams.json` to the C64
   `cocotb/sim_config.json`.

2. Start cosim (it binds the socket and listens):

   ```bash
   make jacquard-cosim        # in the c64-tapeout repo
   # or directly:
   jacquard cosim --config cocotb/sim_config.json <netlist> --top-module chip_top ...
   ```

3. In another terminal, start the renderer:

   ```bash
   uv run docs/spikes/video-streaming-tap/c64_video_renderer.py --socket /tmp/c64-video.sock
   ```

The renderer waits for cosim to listen, connects, and updates the window as
samples stream in (one byte per pixel, framed on hsync/vsync). It auto-reconnects
if cosim drops the client under backpressure.

## Net names (verified against the post-PnR netlist)

The config uses the escaped hierarchical names that survive LibreLane synthesis:

| Role | Net |
|------|-----|
| Pixel strobe (trigger) | `\i_chip_core.c64_u.enablepixel` |
| colorIndex[3:0] | `\bidir_CORE2PAD[1..4]` |
| hsync | `\bidir_CORE2PAD[5]` |
| vsync | `\bidir_CORE2PAD[6]` |

The `--trace-signals` multi-candidate resolver also accepts the plain RTL name
`enablePixel`; the explicit hierarchical form is the safe default. Confirm each
resolves with `netlist-graph search <netlist> <name>` before a run.

## Open question: colorIndex bit order

The renderer assumes `signals` order `colorIndex[3], colorIndex[2],
colorIndex[1], colorIndex[0], hsync, vsync`. If the picture's colours look
permuted, `\bidir_CORE2PAD[1..4]` maps to colorIndex bits in the opposite
order — check the `out_drive` assignment in `chip_core.sv` and swap the
`colorIndex[..]` entries in the config (or adjust `decode_sample` in the
renderer). Wrong order only permutes the palette; it doesn't break framing.
