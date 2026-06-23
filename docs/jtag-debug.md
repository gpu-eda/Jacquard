# Interactive JTAG Debug (`--jtag-server`)

## Overview

`jacquard cosim --jtag-server <PORT>` opens a live
[`remote_bitbang`][openocd-rb] JTAG socket alongside a running GPU
co-simulation. An external debugger — OpenOCD, then gdb on top of it —
attaches and inspects the design through its RISC-V **Debug Module
(DM)**: halt/resume/step the hart, read/write GPRs, CSRs, PC and memory,
and `load` firmware — exactly as the same firmware would be debugged on
real silicon.

It is the **interactive sibling of [`--jtag-replay`](signal-tracing.md)**.
Where `--jtag-replay <FILE>` plays back a *recorded* `remote_bitbang`
byte stream (deterministic, one-directional), `--jtag-server <PORT>`
opens a *live* socket and lets the connected client drive the same
configured TCK/TMS/TDI/(TRST) pins, stepping the design in lock-step with
each debug transaction and answering TDO reads from the design's live
output.

The two are mutually exclusive — pick a recorded stream *or* a live
client, not both.

| Flag | Byte source | TDO read-back | Use |
|------|-------------|---------------|-----|
| `--jtag-replay <FILE>` | recorded file | counted, not answered | deterministic regression / firmware load |
| `--jtag-server <PORT>` | live TCP client | sampled + written to client | interactive debug (OpenOCD → gdb) |

[openocd-rb]: https://github.com/openocd-org/openocd/blob/master/src/jtag/drivers/remote_bitbang.c

## Configuration

The JTAG TAP pins are declared once in `sim_config.json`, shared by both
the replay and server paths. TCK/TMS/TDI/TRST are design **inputs**; TDO
is a design **output**, so the server needs `tdo_gpio` to answer TDO
reads:

```json
{
  "jtag": {
    "tck_gpio": 2,
    "tms_gpio": 3,
    "tdi_gpio": 4,
    "trst_gpio": 5,
    "trst_active_low": true,
    "tdo_gpio": 6
  }
}
```

- `trst_gpio` should be set for a RISC-V Debug TAP: the DTM resets on
  `negedge trst_n`, and the server injects a one-shot power-on TRST pulse
  at startup so a debugger that never asserts TRST (stock OpenOCD with
  `reset_config none`) still resets the DTM — mirroring a real chip's
  power-on TAP reset. (Tunable/disable via the `JACQUARD_JTAG_TRST_PULSE`
  env var; see *How it works*.) TAPs that genuinely reset via a
  five-cycle `TMS=1` sequence can omit it.
- `tdo_gpio` is optional for `--jtag-replay` (replay never answers `R`)
  but **required for useful `--jtag-server` use**: without it, every TDO
  read returns `0` and the debugger cannot read the design back. cosim
  warns at startup if it is missing.

The TCK clock domain must also appear in `clocks[]` so the multi-clock
scheduler allocates it a tick slot; the model overrides TCK at each byte
transition. See `tests/jtag_minimal/sim_config.json` for a complete,
working example.

## Launching the server

```bash
jacquard cosim design.pnl.v \
    --config sim_config.json \
    --top-module top \
    --jtag-server 9999 \
    --jtag-hold-cycles 8 \
    --output-vcd debug.vcd \
    --max-clock-edges 100000000
```

cosim builds the design, **binds `127.0.0.1:9999`, and blocks waiting for
one client** before the run starts:

```
JTAG server `jtag_0`: listening on 127.0.0.1:9999, hold_edges=8 (...);
  waiting for a remote_bitbang client (e.g. OpenOCD)…
```

Pass `--jtag-server 0` to bind an **OS-assigned free port** instead of a
fixed one — read the actual port from the `listening on 127.0.0.1:<port>`
line. Use this when several `jacquard` instances debug concurrently (or
in CI) so they never collide; if a fixed port is already taken, the bind
fails with an actionable error.

Once a client connects, the design steps forward one `remote_bitbang`
transaction at a time, paced by the client. v1 accepts a single
connection; when the client disconnects (or sends `Q`), the session ends
and the simulation free-runs to its edge budget.

> **Edge budget.** `--max-clock-edges` does **not** stop the run while a
> debug client is attached — the session is paced by the client and would
> otherwise die mid-inspect. The cap is honoured again once the client
> disconnects (so the post-session free-run is still bounded). You can
> leave `--max-clock-edges` at its default for interactive use.

## OpenOCD configuration

The boilerplate is the most error-prone part of attaching a debugger, so
generate it with the built-in helper instead of hand-writing it:

```bash
jacquard jtag-openocd-config --port 9999 --expected-id 0xdeadbeef \
    -o openocd.cfg
```

`--irlen` (default 5), `--host`, `--chipname` and `--dmi-timeout` are all
configurable; `jacquard jtag-openocd-config --help` lists them. The
emitted config wires up the `remote_bitbang` driver, a RISC-V Debug TAP,
and the target, and ends with `init`. For reference, the equivalent
hand-written config (mirroring `tests/jtag_minimal/scripts/openocd.cfg`):

```tcl
adapter driver remote_bitbang
remote_bitbang host localhost
remote_bitbang port 9999
transport select jtag
adapter speed 1

set _CHIPNAME jtag
jtag newtap $_CHIPNAME cpu -irlen 5
set _TARGETNAME $_CHIPNAME.cpu
target create $_TARGETNAME riscv -chain-position $_TARGETNAME

# The JTAG protocol is bit-serial and the GPU server is per-edge while
# stepping — keep the DMI timeout generous.
riscv set_command_timeout_sec 60
```

Run it against the launched server:

```bash
openocd -f openocd.cfg
```

OpenOCD examines the TAP, brings up the DM (`dmactive=1`), and exposes a
gdb remote on `:3333` by default.

## Attaching gdb

```bash
riscv32-unknown-elf-gdb firmware.elf
(gdb) target remote :3333
(gdb) info registers          # read GPRs / CSRs / PC via the DM
(gdb) x/16xw 0x20000000       # read memory
(gdb) load                    # write firmware through Program Buffer / sysbus
(gdb) break main
(gdb) continue
```

The design runs on the GPU; gdb sees it through the DM exactly as it
would see silicon over a real JTAG probe.

## How it works

The cosim main loop is synchronous (`step_edge → run_edges → wait →
repeat`). A live debug session **inverts time control**: the client is
the clock source, so `step_edge` *blocking on a socket read is* the
correct synchronisation — no async executor, no background thread.

- While a client is connected the JTAG model reports `is_active() ==
  true`, which forces the existing **single-edge (`batch=1`)** dispatch —
  the same fine-grained stepping `--jtag-replay` already uses. Each cosim
  edge processes one `remote_bitbang` step.
- On each `R` (read-TDO) command the model samples the design's live TDO
  from the output state (`output_state[tdo_pos]`) and writes the ASCII
  `'0'`/`'1'` back over the socket — the only response the protocol
  requires.
- TCK/TMS/TDI/TRST drive through the usual
  `overrides → BitOps → state_prep` path, unchanged from replay.
- **Power-on TRST pulse.** A RISC-V DTM resets on `negedge trst_n`. On
  real silicon the power-on reset supplies that edge; in a recorded
  capture the harness pulses TRST before the debugger runs. But stock
  OpenOCD with `reset_config none` never asserts TRST — so without help
  the DTM would never reset and examine would fail with "dtmcontrol is
  0" / "scan chain interrogation: all zeroes". The live server therefore
  injects one brief TRST pulse at startup (deasserted → asserted →
  released, timed in cosim edges), reproducing the recorded stream's
  leading `u…r`. Replay is unaffected (it drives TRST from the stream),
  and an explicit client TRST assertion still wins after the pulse. The
  window is `[hold_edges, 5·hold_edges)` by default; override or disable
  it with `JACQUARD_JTAG_TRST_PULSE="<from>,<to>"` / `="off"` (edges) if
  a design needs different timing.

The batched fast path is untouched when no client is attached: a design
with a `jtag` config but no `--jtag-server`/`--jtag-replay` flag simply
leaves the JTAG inputs floating and runs at full throughput.

This works on any cosim backend (CPU / Metal / CUDA / HIP) — it is the
CPU-side model plus `batch=1` of the GPU backend, with no per-backend
kernel work. See
[ADR 0017 (Amendment 2026-06-21)](adr/0017-cosim-execution-model.md) for
the execution-model rationale and
[ADR 0013 (Amendment 2026-06-21)](adr/0013-plural-peripheral-configs.md)
for the `tdo_gpio` config surface.

## Caveats and limitations

- **Single client (v1).** One connection per run. Reconnect support and
  multi-tap chains are future work.
- **Performance.** An attached session loses edge batching for its
  duration — inherent and acceptable, since interactive debug is slow
  relative to free-running simulation. Throughput is unaffected when no
  client is attached.
- **X-propagation.** Under `--xprop`, the debug *read* path is two-state
  in v1 (TDO resolves through the design as 0/1). X-aware debug reads are
  a planned refinement (J5 in the plan).
- **Reset interplay.** The design's own reset (`reset_gpio`) and the JTAG
  TAP reset (driven by the client) proceed concurrently, mirroring real
  hardware where the debugger drives JTAG while the chip resets.

## Validation

Three layers, increasing in fidelity:

- A **model-level loopback unit test**
  (`live_source_loops_back_tdo_over_socket` in `src/sim/models/jtag.rs`)
  drives the FSM through a real `TcpStream` and reads back a sampled TDO
  bit — GPU-free and deterministic, runs in `cargo test`.
- The **`jtag-minimal-cosim-server` CI job** streams the *same*
  `bitbang.rec` the `--jtag-replay` gate uses over the live socket (via
  `tests/jtag_minimal/scripts/bitbang_client.py`) and asserts the design
  reaches the same `data0_obs == 0xCAFEBABE`. This pins live-vs-replay
  *drive* equivalence with zero external tooling — but reaches
  `0xCAFEBABE` via a DMI *write*, so it does not exercise a correct TDO
  *read*.
- The **`jtag-minimal-openocd` CI job** drives the live server with
  **real OpenOCD** (`tests/jtag_minimal/scripts/openocd_debug.sh`):
  examine reads IDCODE/DTMCS back over TDO, then it writes *and reads
  back* DATA0, asserting `IDCODE == 0xdeadbeef` and the DMI read-back
  `== 0xcafebabe`. This is the only gate that exercises the live **TDO
  read** path (and the power-on TRST-pulse fix). Run it locally with any
  port:

  ```bash
  jacquard cosim … --jtag-server 9824 &
  bash tests/jtag_minimal/scripts/openocd_debug.sh 9824
  ```

  > **Fake hart.** `jtag_minimal` is a bare DTM+DM with no real CPU, so
  > full RISC-V target examination stops at `Failed to read MISA` and
  > OpenOCD exits non-zero — expected. DMI access (write/read DATA0) is
  > the contract the gate checks. A design with a real hart examines
  > fully.

## References

- Issue [#124](https://github.com/gpu-eda/Jacquard/issues/124).
- Implementation staging:
  [`docs/plans/jtag-debug-server.md`](plans/jtag-debug-server.md).
- [ADR 0017 — Cosim execution model](adr/0017-cosim-execution-model.md)
  (interactive, externally-paced peripheral models; `output_state`
  wiring).
- [ADR 0013 — Cosim peripheral model architecture](adr/0013-plural-peripheral-configs.md)
  (`tdo_gpio` config surface).
- Replay path & fixture: `src/sim/models/jtag.rs`, `tests/jtag_minimal/`.
