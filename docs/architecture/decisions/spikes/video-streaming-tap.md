# Spike — Live video streaming tap for cosim

**Status:** Proposed. Design validated against the C64 tapeout netlist; prototype not yet built.

**Date:** 2026-07-07

## Goal

Let a separate process read a bundle of chip output pins in near-real-time and render them live — the first use being a debug video view of the C64's 4-bit `colorIndex` + `hsync`/`vsync` pads. The tap is observe-only: it samples design outputs and streams them out a socket. It never drives design inputs, so it stays out of the cycle-accurate reactive path.

This came out of a wider question about a "generic GPIO model" for cosim. The two concrete C64 cases split cleanly: video (this spike) is pure output observation, while the CIA1 keyboard/joystick GPIO is the bidirectional tri-state case and is tracked separately (see "Relationship to the GPIO work" below).

## Why not QEMU virtio/vhost

The original idea was to reuse QEMU's virtio-gpio + vhost-user backend. Rejected for this use, for the record so it isn't re-litigated:

- virtio-gpio and vhost-user are untimed and asynchronous ("set line 5 high, eventually"). Cosim is cycle-accurate per clock edge. A live raster needs per-pixel samples in order, which the virtio model throws away.
- virtio-gpio is a logical line-command protocol (get/set value, direction, IRQ). Video out is a wire-level bit bundle sampled at pixel rate. The impedance mismatch is high.
- vhost-user carries real weight (shared-memory negotiation, virtqueue rings, feature negotiation) to expose a semantically poor interface.

virtio-gpio remains the right tool for a different goal — running unmodified guest software against the RTL's GPIO under QEMU — but that is not what a debug video view needs.

## Architecture

The hard part is already solved. When any VCD output is active, cosim enables a device-resident per-edge snapshot ring (`vcd_ring_dev`), blits one full `[input_state | output_state]` snapshot per edge on the GPU, and reads the ring back D2H after each batch. That gives correct per-tick values without forcing `batch=1`. The tap reuses this ring; it adds no GPU code.

A tap is a second sink alongside the output-VCD writer in the per-edge drain loop. For each edge in the batch it extracts the bundle's bits from the snapshot's `output_state`, and (subject to the trigger below) packs one sample and appends it to a per-batch buffer. After the edge loop, the buffer is written to the socket in one non-blocking `write`.

Transport is a UNIX-domain socket. Cosim listens; the renderer connects and disconnects freely. If no client is attached, or the socket would block, cosim drops the batch's samples. Stalling the simulation on a slow renderer is the wrong tradeoff for a debug tool.

Because the tap drives nothing, it does not set `is_active()` and does not force single-edge batches. Throughput is unaffected; end-to-end latency is one batch (`BATCH_SIZE = 1024` scheduler edges by default). Lower latency is a smaller batch, at throughput cost — a knob, not a redesign.

### Wire format

One byte per sample for the video bundle: `[ vsync<<5 | hsync<<4 | colorIndex[3:0] ]`. The stream is contiguous per-tick samples; the renderer reframes from `hsync`/`vsync` edges, so no timestamps are needed in the common case. Each batch write is optionally prefixed with a `u64` tick base so the renderer can resync after a drop. Given drop-on-backpressure, the resync prefix is worth having.

The format generalises: a bundle is an ordered list of signal names plus a socket path, packed LSB-first into as many bytes as the bit count needs. Video is the first consumer; the same sink can later stream CIA ports, a bus address, or anything into Surfer or another live viewer.

## Trigger: sampling at the right rate

`colorIndex` only changes at the ~8 MHz dot rate, so sampling every scheduler edge oversamples and floods the socket with duplicate bytes. A trigger controls when a sample is emitted. Three modes, one field:

- `strobe: "<net>"` — emit on the rising edge of a design net (a pixel clock-enable). The design itself says when a pixel is ready, so this is correct regardless of clock topology. Recommended.
- `clock: "<name>", divider: N` — emit every Nth edge of a named scheduler clock domain. Correct under multiple clock domains because it is domain-relative.
- `every: N` — emit every Nth scheduler edge. Simplest; correct when there is a single master clock.

Default is `every: 1`.

The multi-clock subtlety that makes this worth writing down: a scheduler "edge" is one entry in the interleaved `MultiClockScheduler` schedule, and consecutive edges can belong to different domains. So "every Nth edge" drifts against any one clock the moment there is more than one domain. Both `strobe` and `clock`/`divider` avoid this by binding to something the scheduler tracks per edge. The drain loop already reads exactly this — `scheduler.schedule[sched_idx].domain_edges[di]` tells it which domains fired at each edge (see the jitter code at `mod.rs:3445-3461`). Strobe mode goes one better: it binds to a net the design toggles, so tick topology never enters the reasoning.

## C64 grounding (verified against the netlist)

The C64 core is a single-master-clock design with clock enables, MiSTer style. `clk32` (32 MHz, top-level `clk_PAD`) is the only clock. `enablePixel` is a one-cycle strobe generated in a `rising_edge(clk32)` process, asserted on eight evenly spaced sysCycle states — every 4th `clk32` cycle, so a regular 8 MHz dot strobe (`rtl/c64_system.vhd:224-240`). The multi-clock worry does not apply here; both `strobe` and `every: 4` would work, and `strobe` needs no tuning.

Names survive the LibreLane flow in escaped, lowercased, hierarchical form. Confirmed present in the post-PnR netlist (`librelane/runs/.../chip_top.pnl.v`):

| Purpose | Net in post-PnR netlist | Source |
|---|---|---|
| Pixel strobe (trigger) | `\i_chip_core.c64_u.enablepixel` | `c64_system.vhd:110,228-238` |
| colorIndex[3:0] pad value | `\bidir_CORE2PAD[1..4]` | `chip_core.sv` pad map |
| hsync pad value | `\bidir_CORE2PAD[5]` | `chip_core.sv:63` `PAD_VIC_HSYNC=5` |
| vsync pad value | `\bidir_CORE2PAD[6]` | `chip_core.sv:64` `PAD_VIC_VSYNC=6` |
| per-pad output-enable (Case 2) | `\bidir_CORE2PAD_OE[n]` | pad ring |

`\bidir_CORE2PAD[n]` is the core-driven side of bidir pad `n`, which is what we want for the design's output value. The `--trace-signals` multi-candidate resolver (`src/sim/trace_signals.rs:320` `resolve_to_state_pos`) is built for escaped/flattened/hierarchical names, so passing `enablePixel` or the full hierarchical path should resolve; confirm each with `netlist-graph search` before wiring.

Resulting config for the C64 video tap:

```jsonc
{
  "name": "video",
  "socket": "/tmp/c64-video.sock",
  "trigger": { "strobe": "enablePixel" },
  "signals": ["colorIndex[3]", "colorIndex[2]", "colorIndex[1]", "colorIndex[0]",
              "hsync", "vsync"]
}
```

The renderer is a separate process (Python + an SDL-ish surface, or a small Rust app). It reads the byte stream, frames the raster from `hsync`/`vsync` edges, applies the C64 16-colour palette (`fpga64_rgbcolor.vhd` exists in the RTL as the reference LUT but is not instantiated on-die), and blits live. All video semantics live in the renderer; cosim only forwards bits.

## The Jacquard hook

Everything is host-side in `src/sim/cosim/mod.rs`:

- Per-edge drain loop: `mod.rs:3414-3532`. `backend.vcd_snapshot(edge_in_batch)` returns `[input_state | output_state]`; a bit is `(output_state[pos>>5] >> (pos&31)) & 1`. The tap slots in beside the output-VCD writer at `mod.rs:3438`.
- Output-VCD state / signal→position mapping is built at `mod.rs:2958-2991` via `vcd_io::setup_cosim_output_vcd` → `OutputVCDMapping.out2vcd`. The tap's bundle resolution mirrors this, using `trace_signals::resolve_to_state_pos`.
- The snapshot ring is gated by `vcd_enabled` (`mod.rs:2998`) / `enable_vcd_ring` on the `CosimBackend` trait. The tap must ensure the ring is enabled even when no output VCD path is set.

Config surface: extend `TestbenchConfig` (`src/testbench.rs`) with a `video_taps` / `signal_streams` vector, following the plural-peripheral convention (Decision 0013). A CLI `--video-stream` shorthand is optional sugar.

Rough size: about 40 lines of Rust for the sink plus config plumbing, and about 60 lines for a minimal renderer.

## Spike artifacts

The tap is implemented in-tree (`src/sim/cosim/signal_stream.rs`, config in
`src/testbench.rs`). The C64-specific renderer and config fragment live under
`video-streaming-tap/` next to this doc — a `uv`-runnable pygame renderer and
the `signal_streams` block to merge into the C64 `cocotb/sim_config.json`. They
are C64-specific and will move to the c64-tapeout project; they sit here so the
spike runs end-to-end. See `video-streaming-tap/README.md`.

## Open questions and next steps

1. Run it end-to-end: a real cosim run streaming to the renderer, confirming the C64 net names resolve. The tap and renderer exist; this is the remaining validation.
2. Confirm `colorIndex` bit order at the pad — whether `\bidir_CORE2PAD[1]` is `colorIndex[0]` or `[3]` — against the `out_drive` assignment in `chip_core.sv`. Cheap to check; wrong order just permutes the palette.
3. Backpressure policy: drop whole batches vs a bounded ring with newest-wins. Start with drop-whole-batch; revisit if the picture tears badly.
4. Whether to land the generic "signal-bundle stream" surface now or start video-specific and generalise once a second consumer appears. Leaning generic, since the CIA case will want it.

## Relationship to the GPIO work

This spike is Case 1 (output observation) of the broader generic-GPIO investigation. Case 2 — the CIA1 PA/PB keyboard-matrix and joystick GPIO — is the bidirectional tri-state case and is separate work: it drives inputs reactively, needs `is_active()`/`batch=1`, and needs open-drain + pull-up wired-AND resolution. This spike incidentally found the net Case 2 will need: per-pad output-enable is `\bidir_CORE2PAD_OE[n]` at the synthesized top, which answers "how is the CIA tri-state `$oe` expressed" for the `GpioMapping` plumbing when that work starts.
