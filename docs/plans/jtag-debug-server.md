# Interactive JTAG/DM debug server (`--jtag-server`) — plan

**Status:** J1–J4 implemented (interactive `--jtag-server` lands; J5
single-step/breakpoints deferred). User guide: [`docs/jtag-debug.md`](../jtag-debug.md).
Tracks [#124](https://github.com/gpu-eda/Jacquard/issues/124).
**Goal:** add an interactive JTAG debug server to `jacquard cosim` so an
external debugger (OpenOCD → gdb) can attach to a running GPU co-simulation and
inspect the design through its RISC-V **Debug Module** — read/halt/resume/step
architected state (GPRs, CSRs, PC, memory), exactly as the same firmware would
be debugged on real silicon.

`--jtag-server <port>` is the **interactive sibling** of the existing
`--jtag-replay <stream>`: instead of replaying a recorded `remote_bitbang` byte
stream, it opens a live `remote_bitbang` TCP socket and drives the same
configured TCK/TMS/TDI/TRST pins from the connected client, stepping the design
in lock-step with debug transactions.

## Why this is cheap: the infrastructure already exists

cosim already drives a design's DTM/DM via `--jtag-replay` to load firmware.
The same DTM/DM, fed a *live* bitbang socket instead of a recording, exposes
the full RISC-V external-debug interface. The architectural pieces are in place:

- **`JtagReplayModel`** (`src/sim/models/jtag.rs`) already parses the entire
  `remote_bitbang` byte alphabet (`0`–`7` pin drives, `r/s/t/u` reset, `R` TDO
  read, `B/b` blink, `Q` quit) and maps it to TCK/TMS/TDI/TRST drives.
- **`PeripheralModel`** (`src/sim/models/mod.rs:56`) is already the right
  contract: `step_edge(output_state, overrides, emitted)` is bidirectional, and
  **`is_active()` already forces `batch=1`** (single-edge dispatch) while JTAG
  is live — exactly the fine-grained stepping a debug session needs. The
  batched fast path is untouched when no client is attached.
- cosim already resolves the `jtag` peripheral's pin mapping from
  `sim_config.json` and patches model-driven pins into the per-edge `BitOp`
  ops via `state_prep`.

So the live server is largely **"swap the recorded `Vec<u8>` cursor for a live
socket, and answer `R` from real TDO."** No GPU/kernel change, no backend
change, no `PeripheralModel` trait change.

## Seam map (verified, with anchors)

| Concern | Location | Today | Needed |
|---|---|---|---|
| Byte alphabet → pins | `src/sim/models/jtag.rs:156-196` (`consume_byte`) | parses `0-7,r,s,t,u,R,B,b,Q`; `R` only counted, `B/b` no-op | answer `R` with live TDO |
| Byte source | `jtag.rs:74-77` (`bytes: Vec<u8>`, `cursor`) | hardcoded Vec + cursor | abstract over `Replay(Vec)` vs `Live(TcpStream)` |
| Edge advance | `jtag.rs:229-251` (`step_edge`) | `bytes[cursor]`; one-edge TCK deferral for TMS/TDI settle | `source.next_byte_blocking()` |
| Observe half | `cosim/mod.rs:2987` | `step_edge` gets `&[]` — **TDO not wired** | pass `&backend.state()[state_size..]` |
| Drive path | `cosim/mod.rs` `contribute_overrides`→`overrides`→`patch_model_driven_in_ops` (`1916-1929`) | works for replay | **unchanged** |
| `is_active()`→batch=1 | `jtag.rs:310-312`, gate at `cosim/mod.rs:3042-3045` | `!finished()` | live model: true while connected |
| Instantiation | `cosim/mod.rs:2354-2415` | reads file → `JtagReplayModel::new` | branch on `jtag_server`: bind + `accept()` |
| Config | `src/testbench.rs:361-374` (`JtagConfig`) | `tck/tms/tdi/trst_gpio` (inputs only) | add `tdo_gpio: Option<usize>` (an output) |
| CLI/opts | `jacquard.rs:362-375`, `:2007-2008`; `CosimOpts` `cosim/mod.rs:49-52` | `jtag_replay`, `jtag_hold_cycles` | add `jtag_server: Option<u16>` |
| Config example | `tests/jtag_minimal/sim_config.json:15-21` | `jtag{}` + `clocks[]` TCK domain | + `tdo_gpio` |

## Design decisions

- **D1 — Single-threaded blocking socket; no async, no thread.** The cosim loop
  is synchronous (`step_edge` → `run_edges` → `wait` → repeat). A live debug
  session inverts time control: the **client (OpenOCD) drives the pace**, so
  `step_edge` blocking on a socket read *is* the correct synchronisation. With
  `is_active()==true` forcing `batch=1`, each edge processes one bitbang step.
  No executor or background thread is required for a single connection.

- **D2 — Wire `output_state` into `step_edge` (resolve the standing TODO).**
  Both Decision 0017 and Decision 0013 note `step_edge` is handed an empty `output_state`
  "until I²C/SPI observation needs it." The interactive server is the first CPU
  model that *must* read a design output (TDO). Replace the `&[]` at
  `cosim/mod.rs:2987` with the real output slice. This also unblocks the
  scaffolded I²C/SPI models — a general improvement, not a JTAG special case.

- **D3 — Abstract the byte source, not the model.** Introduce
  `enum JtagSource { Replay { bytes: Vec<u8>, cursor: usize }, Live(TcpStream) }`
  in `jtag.rs`; the single change point is `bytes[cursor]` →
  `source.next_byte_blocking()`. `JtagReplayModel` keeps its FSM; only its
  input feed changes. Replay behaviour is byte-for-byte identical.

- **D4 — TDO read-back over the socket.** On `R`, sample TDO from
  `output_state[tdo_pos]` and write the ASCII `'0'`/`'1'` back to the
  `TcpStream` — the only response the `remote_bitbang` protocol requires.
  `tdo_pos` is resolved from the new `JtagConfig.tdo_gpio` via the design's
  output-bit map after construction.

- **D5 — `--jtag-server <PORT>`; mutually exclusive with `--jtag-replay`.** Bind
  a `TcpListener` and `accept()` (blocking) in the instantiation block *before*
  the main loop — single connection for v1, stored in the model. `Q` ends the
  session; the sim can continue free-running (decision: log and continue, vs
  exit — see open questions).

- **D6 — Honour the OpenOCD `remote_bitbang` contract exactly.** Reuse the
  existing alphabet handling; `B/b` (blink) stay no-ops, `R` is the sole
  response byte, `s/u` (SRST) map to the existing reset handling. Mirrors
  OpenOCD's `remote_bitbang.c` so stock OpenOCD connects unmodified.

## Validation strategy

Real OpenOCD+gdb attach is interactive and heavy for CI, so the **CI gate is a
self-contained loopback test**, with manual OpenOCD as a documented recipe:

- **V1 — loopback integration test (CI).** A Rust test acts as the
  `remote_bitbang` *client* over a localhost socket: it feeds the **same
  recorded stream** the `jtag_minimal` fixture already uses (`bitbang.rec`),
  services `R` reads, and asserts the design reaches the **same
  `data0_obs == 0xCAFEBABE`** as the replay path. This exercises the full live
  socket → TDO read-back → drive path end-to-end with **zero external tooling**,
  and pins live-vs-replay equivalence. Runs on the CPU/Metal cosim backends like
  the existing `jtag-minimal-cosim` job.
- **V2 — manual OpenOCD + gdb recipe (docs).** A `docs/jtag-debug.md` guide:
  `jacquard cosim … --jtag-server 9999`, an OpenOCD `remote_bitbang` config,
  `gdb … target remote`, `info registers` / `x/` / `load`. The issue names the
  cocotb `RemoteBitbangServer` as the behavioural precedent to match.

## Staged checkpoints (each ≈ one reviewable PR)

| # | Scope | Gate | Status |
|---|---|---|---|
| J1 | Wire `output_state` into `step_edge` (D2); add `JtagConfig.tdo_gpio` + resolve `tdo_pos`; unit-test TDO sampling in the replay model | replay fixtures still byte-identical; TDO-sample unit test green | ✅ done |
| J2 | Byte-source abstraction (D3): `JtagSource` enum; replay path refactored onto it | `jtag_minimal` replay unchanged; model unit tests green | ✅ done |
| J3 | `--jtag-server` (D5) + live byte source + `R` write-back (D4) + `TcpListener` accept | **V1 loopback test**: live run == replay golden (`data0_obs==0xCAFEBABE`) | ✅ done (model loopback unit test + `jtag-minimal-cosim-server` CI gate; verified locally on Metal) |
| J4 | `docs/jtag-debug.md` (V2 recipe); `--help` text; cross-link decisions | docs build; manual OpenOCD/gdb smoke (local) | ✅ done (guide + `--help`; manual OpenOCD/gdb left to operators) |
| J5 (later) | Single-step / breakpoints via DM `step`/triggers; X-aware debug under `--xprop` | follows from attach; see open questions | ⏳ deferred |

CUDA/HIP note: the interactive path is the CPU-side model + `batch=1` of the
GPU backend (per Decision 0017's "per-edge fallback"), so it works on any cosim
backend once J1–J3 land; no per-backend kernel work.

## Risks / open questions

- **TDO sample timing.** `R` must sample TDO at the protocol-correct point
  (after the TCK edge the client just clocked). The model already defers TCK by
  one edge (`pending_tck`) for TMS/TDI settle; confirm the output slot read on
  `R` reflects the just-clocked state. The loopback test (V1) catches
  misalignment via the `data0_obs` assert.
- **Performance.** Interactive sessions lose edge batching for their duration
  (inherent and acceptable — debug is slow). The batched fast path is
  unaffected when no client is attached.
- **`--xprop` interaction.** The DM debug-*load* path's X-behaviour was
  addressed in [#102](https://github.com/gpu-eda/Jacquard/issues/102). Initial
  debug runs two-state; X-aware *read* under `--xprop` is a J5 refinement, not
  the initial ask.
- **Single vs multi client / session end.** v1 is one connection; `Q` either
  exits cosim or lets it free-run — pick the gdb-friendly behaviour during J3.
- **`--max-clock-edges` semantics.** An attached session may outlive the edge
  budget; decide whether `--jtag-server` disables/relaxes the cap while a client
  is connected.

## References

- Issue [#124](https://github.com/gpu-eda/Jacquard/issues/124).
- Decision 0017 (cosim execution model) — Amendment 2026-06-21 (interactive,
  externally-paced peripheral models; `output_state` wiring).
- Decision 0013 (peripheral model architecture) — Amendment 2026-06-21 (`tdo_gpio`
  config surface; `--jtag-server` as the interactive sibling of `--jtag-replay`).
- Existing replay path: `src/sim/models/jtag.rs`, `tests/jtag_minimal/`
  (`bitbang.rec`, `sim_config.json`, the `jtag-minimal-cosim` CI gate).
