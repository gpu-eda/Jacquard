# Handoff — backend alignment (CUDA/HIP/Metal) + cosim portability

**Created:** 2026-06-07 (updated 2026-06-17, eighth session)
**Branch:** `cuda-hip-parity` (PR #120, off `main`). **PHASE 1 MERGED TO MAIN**
(#118 rebase-merged @ `main` `e77aab3`; Phase 0 + Phase 1 steps 1–7,
`CpuBackend` functional parity, `cosim-cpu` Linux CI + 7 golden fixtures all in
`main`). Now on the **CUDA/HIP parity track for release**: scope is **#104 (sim
timing) + cosim Phase 2 (full batched, 2a+2b)** — decided with maintainer.
**Track 0 (#104) code DONE + pushed** (commits `cf3ca15` plan, `69afd28` #104
wiring), under first-ever T4 compile validation on PR #120. Working tree: clean
after commit. Authoritative plan: `docs/plans/cosim-phase2-cuda-hip.md`.

## Goal & next-up

**Goal:** bring CUDA/HIP up to Metal parity for release. Tracks: (a) `sim`
kernel cross-backend *equivalence* (done — CI guard); (b) **cosim portability
(#105)** — Phase 0+1 DONE & **merged to main**; (c) **#104 sim timing** —
Track 0 code landed on `cuda-hip-parity`, validating on T4; (d) **cosim Phase 2**
(CUDA/HIP cosim backend + Tier-2 GPU peripherals) — next.

**Now (pick up here):**
1. **Confirm PR #120 CI green on the T4** — the `cuda` + `hip-on-nvidia` jobs are
   the first real `nvcc`/`hipcc` compile of the #104 Rust wiring (the binding
   types + timed-launcher call were written blind; dev machine is Metal-only).
   Watch run for the `cuda-hip-parity` push; run id in
   `/tmp/claude/cudahip-runid.txt`. If red, fix the CUDA/HIP-specific compile
   error and re-push.
2. **Add the #104 CI timing-equivalence check** (Track 0 follow-up): extend the
   `cuda`/`hip` jobs to run a timing fixture (`tests/timing_test/dff_test_synth.gv`
   + constraints) and assert the report matches Metal; add the timing VCD to
   `backend-equivalence`.
3. **Then cosim Phase 2** — checkpoints 2a (per-stage `simulate` + `state_prep`
   CUDA/HIP kernels, `CudaBackend`/`HipBackend` with managed memory + CPU
   peripherals, per-edge) then 2b (port the 3 GPU peripheral kernels for
   batching). Full checkpoint table + the authoritative Metal-cosim port spec are
   in `docs/plans/cosim-phase2-cuda-hip.md`. CPU goldens (`tests/*/expected/`) +
   the `cosim-cpu` job are the equivalence oracle.

**#104 status (DONE locally, code on `cuda-hip-parity`):** `sim_cuda`/`sim_hip`
were calling the untimed `simple_scan` launcher and dropping `timing_constraints`
on the floor. The kernel-side timing logic is **already** in the shared
`kernel_v1_impl.cuh` (arrival writeback :530, setup/hold `write_event` :546-553)
and the timed C launchers (`simulate_v1_noninteractive_timed_{cuda,hip}`,
`kernel_v1.cu:50`/`.hip.cpp:70`) already exist — `ucc::bindgen` auto-surfaces
them as `simulate_v1_noninteractive_timed`. The fix is pure Rust (~120 lines):
widened `TimingReportConfig`/`report_cfg` cfg-gates to any-GPU; timed branch in
`sim_cuda`/`sim_hip` (EventBuffer marshalled as `UVec<u8>`, drained ONCE post-run
since CUDA/HIP are a single bulk cooperative launch — events are cycle-stamped by
the kernel); factored `TimingReportConfig::{make_builder,emit_report}` shared by
all 3 backends. Local gates: default + Metal build clean, 298 Metal tests pass.

**Phase-2 cosim kernel landscape (verified 2026-06-17):** CUDA/HIP have the
`sim` cooperative scan only — **zero cosim kernels**. Metal has the full suite
(`simulate_v1_stage`, `state_prep`, `gpu_apply_flash_din`,
`gpu_flash_model_step`, `gpu_io_step`). `cmd_cosim` dispatches metal→MetalBackend,
else→CpuBackend, so a `--features cuda` build today runs cosim on CPU. Build
mechanism: add `extern "C"` `_cuda`/`_hip` launchers to `kernel_v1.cu`/`.hip.cpp`
(+ `__global__` in shared `kernel_v1_impl.cuh`); `ucc::bindgen` auto-generates
bindings, no build.rs change. Unified memory: Metal `StorageModeShared` → CUDA
`cudaMallocManaged` (closest analog; keeps backend struct ≈ MetalBackend).

**Step 7 — DONE (`e530f32`):** `scripts/ci/cosim_cpu_check.sh` builds the no-GPU
binary (`cargo build -r --bin jacquard`, no features), runs all 7 cosim fixtures
`{xprop_cosim (4 variants), dual_uart, apb_trace (±xprop)}` via `CpuBackend`, and
diffs against committed goldens under `tests/{xprop_cosim,dual_uart,apb_trace}/
expected/`. The `cosim-cpu` job (`ubuntu-latest`, free runner) in
`.github/workflows/ci.yml` runs it on every push. `.gitignore` carries a scoped
`!tests/xprop_cosim/expected/*.vcd` negation so the goldens are tracked.
**Verification harness reference:** `/tmp/claude/cosim_fixtures.sh` runs all 7 on
a Metal binary; the committed CI variant drops `--features metal`.

**Older step-5 notes (now partly done in 5a) below.** Plan: step 5 bullet in
`docs/plans/cosim-phase1-cpu-backend.md`. `Vec<u32>` state sized
`effective_state_size()*2`, `Vec<u32>` sram, `Vec<Vec<BitOp>>` schedule.
`run_edges` (blocks × `num_major_stages`) via
`cpu_reference::simulate_block_v1`; flash via `CppSpiFlash::step` (internal,
injects `d_i` into input state); bus via `BusTraceDecoder`; **new CPU UART-TX
decoder** mirroring `UartDecoderState`'s FSM. `CpuBackend::new` **asserts**
`!script.timing_arrivals_enabled` and `!(xprop_enabled && sram_storage_size>0)`
(see plan Risks). `--check-with-cpu` becomes a no-op-with-warning under
CpuBackend (the backend *is* the reference). The trait is fully backend-neutral
after step 3: `CpuBackend` implements `new`/`init_schedule`/`edge_ops`(`_mut`)/
`edges_per_period`/`gcd_ps`/`run_edges`/`wait`/`state`(`_mut`)/`sram`/`flash_d_i`/
`flash_set_in_reset`/`drain_uart_tx`/`drain_bus_beats`/`vcd_snapshot`/
`enable_vcd_ring` — the debug/profile methods (`profile_kernels`,
`debug_flash_raw_tick0`, `drain_wb_trace_debug`, `uart_decoder_debug`,
`flash_debug_snapshot`) have **no-op/default trait bodies** so come free.
**`bus_lanes` for `CpuBackend::new`:** `build_bus_trace_params` currently lives in
`cosim/metal.rs` (returns a GPU struct + lanes) — extract a lanes-only agnostic
helper into `mod.rs` so `CpuBackend::new` can build lanes without metal.
**When CpuBackend consumes the agnostic surface, drop the temporary
`#![cfg_attr(not(feature="metal"), allow(dead_code))]` at `cosim/mod.rs:18`.**
Then steps 6–7 (`cmd_cosim` backend selection + Linux cosim CI on `ubuntu-latest`).

**Step-5 reference implementations** (kraken should read these): the
`--check-with-cpu` CPU-stepper block in `run_cosim_generic` (`cosim/mod.rs`
~`:2546–2600`: CPU state_prep = copy output→input + apply edge `BitOp`s, then
`apply_flash_din`, then `simulate_block_v1`) is the working prototype for
`CpuBackend::run_edges`. `cpu_reference::simulate_block_v1` (`cpu_reference.rs:17`;
`_xprop` variant `:299`). `CppSpiFlash` (`testbench.rs:45`, FFI `spiflash_step(clk,
csn, d_o) -> u8` returns MISO `d_i`). `BusTraceDecoder::push(RawBeat) ->
Option<BusTransaction>` (`models/bus_trace.rs:121`). **The UART-TX decode FSM
lives in the Metal shader (`gpu_io_step` in `csrc/kernel_v1.metal`), NOT in Rust**
— the CPU port must mirror that shift-register/baud FSM (the GPU mirror struct
`UartDecoderState` is at `cosim/metal.rs:104`); equivalence-test it against
Metal's `dual_uart` output.

**Step-5 verification couples with step 6:** `CpuBackend` can't be *run*
end-to-end until `cmd_cosim` selects it (step 6). Options: (a) do 5+6 together so
the existing fixtures harness can run `{xprop_cosim, dual_uart, apb_trace}` on a
no-GPU build and compare to the Metal golden (most robust — the byte-identical
cross-backend assertion the plan calls for); or (b) land step 5 with only a
compile gate (`cargo build --no-default-features`) + a focused unit test for the
new CPU UART-TX FSM, deferring runtime verification to step 6. The Phase-1
fixtures are chosen so CpuBackend output should be byte-identical to the Metal
golden (no timing, no SRAM-xprop — both asserted off in `CpuBackend::new`).

**Done so far in Phase 1:**
- **Step 1** (`bc18f79`): de-Metaled the `run_edges` seam (dropped
  `metal::Buffer`), moved the VCD ring into `MetalBackend`
  (`enable_vcd_ring`/`vcd_snapshot`), added `flash_set_in_reset`.
- **Step 2a** (`75c9ec0`): extracted flash buffers → `MetalBackend::build_flash_buffers`.
- **Step 2b** (`a93812a`): extracted gpu_io_step (uart/wb/bus) buffers →
  `MetalBackend::build_io_buffers` (returns 7 buffers + CPU `bus_lanes`).
- **Step 2c** (`9593a9f`): extracted state/sram/sram-xmask/event/blocks buffer
  setup → `MetalBackend::build_state_buffers` (`cosim_metal.rs:2603`), a static
  fn `(device, script, config, state_size)` returning a 7-tuple incl.
  `event_buffer_ptr` (caller keeps the two `drop(Box::from_raw(...))` sites).
  Moved: states alloc+`fill(0)`, xprop X-mask seed (both slots), sram
  data/xmask alloc+fill, SRAM ELF preload, blocks no-copy, leaked-`Box` event
  buffer. **Stayed in `run_cosim`** (agnostic, for CpuBackend reuse in step 3):
  `reg_init`/reset/constant_ports/`set_flash_din` stimulus deposits,
  `sram_dumper`, `timing_constraints_buffer`. Metal bit-identical, 298 tests
  pass. Blocks/event hoisted earlier than before — bit-identical (no `states`
  dep, harness-confirmed).
- **Step 3a** (`d5a029f`): added `CosimBackend::{state,state_mut,sram}` +
  `MetalBackend.sram_len`; routed the ~15 read-only `states_buffer`/
  `sram_data_buffer` loop-body reads through `state()`/`sram()`. `run_cosim`
  stays concrete-typed.
- **Step 3b-i** (`32d31b8`): decoded-records seam (ADR 0017 Layer 3). Added
  `CosimBackend::{flash_d_i, flash_debug_snapshot, drain_uart_tx,
  drain_bus_beats, drain_wb_trace_debug, uart_decoder_debug,
  debug_flash_raw_tick0}` + `FlashDebug` agnostic struct. Flash const-params
  (`FlashModelParams`/`FlashDinParams`) became agnostic locals in `run_cosim`
  (clk/csn/d_out_pos, has_flash, d_in_pos); uart/wb/bus ring read-cursors
  (`uart_read_heads`/`wb_trace_read_head`/`bus_trace_read_head` + `n_uarts`)
  moved into `MetalBackend`. **`run_cosim` body now has zero
  `backend.<field>.contents()` reads** but is still concrete-typed (the generic
  flip is 3b-ii). Metal bit-identical, 298 tests pass. (Deviations, all
  bit-identical: added `flash_d_out_pos` local for the trace dump; 3 `FlashDebug`
  fields carried `#[allow(dead_code)]` for the CpuBackend contract; tick-0
  raw+field dump consolidated into `debug_flash_raw_tick0`, stderr-only.)
- **Step 3b-ii-a** (`0bb150c`): assembled `MetalBackend::new(...) -> (Self,
  Vec<BusTraceLane>)` (fat constructor: `MetalSimulator::new` + build_state/flash/io
  + timing buffer + struct literal). Stimulus deposits now go through
  `state_mut()` after construction. `event_buffer_ptr` became a field freed by a
  `Drop for MetalBackend`, removing the two manual `drop(Box::from_raw(...))`
  sites. `run_cosim` stayed concrete. Profile early-return Drop path explicitly
  exercised; no double-free. Metal bit-identical, 298 tests pass.
- **Step 3b-ii-b** (`14f6a27`): added `CosimBackend::new` (fat constructor) +
  `profile_kernels` (default no-op) to the trait; moved the `write_params`
  GPU-setup loop into `MetalBackend::new`. Renamed the body to
  `run_cosim_generic<B: CosimBackend>` (all `backend.*` via trait methods — zero
  concrete `MetalBackend`/`metal::` tokens); `pub fn run_cosim` is now a thin
  `run_cosim_generic::<MetalBackend>` shim, so `jacquard.rs:1792` is unchanged
  and `MetalBackend` stays private. **Step 3 done.** Metal bit-identical, 298
  tests pass.
- **Step 4** (`0327080`): module split `cosim_metal.rs` →
  `src/sim/cosim/{mod,metal}.rs`. `mod.rs` (3298 lines, **non-gated**):
  `CosimBackend` trait, `BitOp`/`FlashDebug`, `CosimOpts`/`CosimResult`,
  `run_cosim_generic<B>`, the patchers, the multi-clock scheduler, CPU-baseline
  helpers, `set_bit`/`clear_bit`/`set_flash_din`/`build_gpio_mapping`/
  `BusTraceLane`, agnostic glue. `metal.rs` (2318 lines,
  `#[cfg(feature="metal")]`): `MetalBackend`(+`Drop`)/`MetalSimulator`/
  `ScheduleBuffers`, all GPU `#[repr(C)]` structs, `build_*`/`encode_*`/
  `write_params`/`create_*`, `build_wb/bus_trace_params`, the `run_cosim` shim.
  `src/sim/mod.rs` now `pub mod cosim;` (non-gated); `jacquard.rs` paths
  `cosim_metal::` → `cosim::`. **`cargo check --lib --no-default-features` now
  compiles the agnostic half** (the real new gate). Metal bit-identical, 298
  tests pass. (Deviations: `StatePrepParams` kept in `metal.rs` (GPU-ABI, sole
  user is metal); temporary `#![cfg_attr(not(feature="metal"),
  allow(dead_code))]` in `mod.rs:18` until `CpuBackend` consumes the surface.)
- **Step 5a** (`b4a9bea`): `CpuBackend` (logic subset) + no-GPU `cmd_cosim`
  wiring. `CpuBackend` in `cosim/mod.rs` (non-gated): `run_edges` via
  `cpu_reference::simulate_block_v1[_xprop]` mirroring the `--check-with-cpu`
  stepper; **xprop X-mask state_prep ported from `kernel_v1.metal:679–715`**
  (output→input copy + per-BitOp value set + driven-bit xmask clear at
  `xmask_state_offset`). Scope-guard asserts in `new`: `!timing_arrivals_enabled`,
  `!(xprop_enabled && sram_storage_size>0)`. Interior mutability via
  `UnsafeCell<Vec<u32>>` (state/sram/sram_xmask/vcd_ring) because trait
  `run_edges` is `&self` (sound — sequential dispatch; `state_mut` uses
  `get_mut`). **`run_cosim_cpu` pub shim** (`run_cosim_generic::<CpuBackend>`);
  `cmd_cosim`'s `#[cfg(not(feature="metal"))]` branch now runs cosim on CPU
  (hard-error removed; shared setup de-gated, only final dispatch gated). Flash
  (`flash_d_i`→0x0F, `drain_*`→empty) + `bus_lanes`→empty stubbed for 5b/5c.
  Dropped the temporary `allow(dead_code)`. **Verified: the 4 logic VCDs
  (`xprop`/`2state`/`noreginit`/`reginit`) byte-identical to the Metal golden on
  a no-feature build** (xprop matched — X-mask port correct); Metal path
  unchanged (7/7 bit-identical), 298 tests pass. ⚠️ Review note: the `UnsafeCell`
  interior mutability is pragmatic (matches Metal's `&self` run_edges) — a
  candidate for a cleaner pattern, or change the trait `run_edges` to `&mut self`
  (would touch MetalBackend too).
- **Step 5b** (`4140ae0`): ported the UART-TX decoder FSM
  (`kernel_v1.metal:1189–1249`) to `CpuBackend`. Per-channel 4-state decoder
  (`CpuUartConfig` tx_out_pos/cycles_per_bit built in `new` mirroring
  `build_io_buffers`' `UartParams`; `CpuUartDecoderState` mirroring the GPU
  struct, `last_tx=1` init), run **per-edge in `run_edges` after simulate**
  (matches Metal's `encode_io_step` cadence — `gpu_io_step` runs once per edge,
  `current_cycle += 1`/edge), accumulating completed bytes drained by
  `drain_uart_tx` (`std::mem::take`). Byte-identical because both backends share
  the agnostic batch-size policy (timestamps align) + identical FSM.
  **`dual_uart_events.json` byte-identical to golden** on a no-GPU build; 4 logic
  VCDs still OK; Metal 7/7; 298 tests. (Stale rust-analyzer dead_code warnings
  appeared mid-edit; `cargo check` on the commit is clean — FSM is wired.)
- **Step 5c** (`a525a25`): ported the APB3 bus-trace beat extraction
  (`kernel_v1.metal:1305–1352`) to `CpuBackend` + shared the bus-lanes builder.
  Extracted agnostic `build_bus_trace(...) -> (Vec<BusTracePositions>,
  Vec<BusTraceLane>)` + the `BUS_TRACE_MAX_*`/`MAX_BUS_TRACES` consts into
  `cosim/mod.rs`; `metal.rs`'s `build_bus_trace_params` calls it and packs into
  the GPU `BusTraceParamsAll` (Metal bit-identical). `CpuBackend` gained
  `bus_positions` + per-edge FSM (`bus_prev_gate`/`bus_current_tick`/`bus_beats`,
  after the UART FSM) emitting `RawBeat`s (same `flags` mapping as
  `MetalBackend::drain_bus_beats`); `new` returns real `bus_lanes`. **ALL 7
  CpuBackend fixtures byte-identical to the Metal golden on a no-GPU build**;
  Metal 7/7; 298 tests. **CpuBackend Phase-1 functional parity complete.**

- **Step 7** (`e530f32`): Linux cosim regression CI. `scripts/ci/cosim_cpu_check.sh`
  runs all 7 fixtures via the no-GPU `CpuBackend` build and diffs against committed
  goldens (`tests/{xprop_cosim,dual_uart,apb_trace}/expected/`); new `cosim-cpu`
  `ubuntu-latest` job in `.github/workflows/ci.yml` wires it into every push;
  `.gitignore` negation tracks the `.vcd` goldens. CI fully green on `e530f32`
  incl. this job. **Phase 1 DONE.**

**Then:** Phase 2 — CUDA/HIP cosim backend + Tier-2 GPU peripherals (not started).

**Bit-identical gate:** `/tmp/claude/cosim_fixtures.sh <out>` + `shasum -c
/tmp/claude/golden.sums` after every step (all green through P1.2c). **Note:**
`run_params.json` was dropped from `golden.sums` this session — it carries a
per-run random `master_seed` (`run_params.rs:12`) so it could never be
bit-identical; the 7 meaningful artifacts (4 VCDs, UART events, 2 APB CSVs) are
the gate. Two Phase-0 deferrals are folded into the Phase 1 steps above (see plan).

**Phase 0 — what landed (branch `cosim-backend-seam-phase0`):**
- `bda27ce` — factored the repeated `*mut BitOp` shared-memory slice behind
  `ScheduleBuffers::edge_ops_mut`/`edge_ops` (the plan's "main friction").
- `cf053b7` — extracted the `CosimBackend` trait + `MetalBackend` struct.
  `MetalBackend` owns the `MetalSimulator`, the `[2×state_size]` state buffer,
  the per-edge schedule (built once via `init_schedule` from a backend-agnostic
  `Vec<Vec<BitOp>>`), and all ~18 GPU IO buffers. `run_edges`/`profile_kernels`/
  `wait` **forward to the UNCHANGED** `encode_and_commit_gpu_batch`/
  `profile_gpu_kernels`/`spin_wait` (GPU-encoding logic byte-identical). The
  three ops-patching closures became free fns taking `&mut dyn CosimBackend`
  (closure-borrow resolved). `edge_ops_mut` is `&mut self` (compile-time
  exclusivity over interior-mutable Metal shared memory).

**Phase-0 deferrals → do in Phase 1 (both noted in-code):**
- **`state()`/`state_mut()`** (the plan's `output_state`/`input_state_mut`)
  were omitted — Metal reads state via the VCD ring + direct buffer access, so
  they were dead on Metal. `CpuBackend` needs them (CPU-side `state_prep`);
  add them to the trait then.
- **`run_edges(vcd_ring: Option<&metal::Buffer>)` leaks a Metal type** into the
  agnostic trait. When `CpuBackend` lands, make the per-edge output-snapshot
  ring a backend-owned buffer with a `&[u32]` drain accessor so the param drops
  out of the trait.
- The module split to `cosim/{mod,metal}.rs` remains separable/cosmetic.

**Bit-identical harness ready:** `/tmp/claude/cosim_fixtures.sh <outdir>`
runs all 7 Metal cosim fixtures; golden baseline at `/tmp/claude/golden`
(sums in `/tmp/claude/golden.sums`). Re-run + `shasum -c` after any refactor
step. (Verified byte-identical after both Phase 0 commits.)

**Verify:** `cargo test --lib --features metal` (298 pass). Metal cosim
parity via the fixtures script (all byte-identical to golden) +
`jtag_minimal` 4M-edge replay PASS (`data0_obs=0xCAFEBABE`, exercises the
model-driven-clock per-edge path).

## Done this session (2nd session, 2026-06-07)

- **#115 merged** (rebase-merge → `main` @ `1371c52`). CPU
  `simulate_block_v1` consolidation; CI green pre-merge.
- **#116 merged** — `fix(cpu_reference)`: `mask & mask.wrapping_neg()`
  replaces `mask & (-(mask as i32)) as u32` (2 sites in `cpu_reference.rs`),
  fixing the debug-build "negate with overflow" panic on bit 31 → makes
  `cosim --check-with-cpu` runnable in debug.
- **#117 merged** — `feat(cosim)`, commits: (1) batch-utilisation telemetry
  in the run summary (behaviour-preserving, 7 fixtures byte-identical);
  (2) rename `edges_per_sys_clk*` → `sched_ticks_per_sys_clk*` + comment fix
  (counts dense scheduler ticks per sys_clk period, not the always-2
  transitions); (3) **ADR 0017 target-architecture rewrite** + plan staging,
  incl. review refinements (backend-owned schedule / `edge_ops_mut`; merged
  P2/P3). **Key data:** GPU-peripheral designs run **100% batched**;
  `jtag_minimal` batches 97.4% of edges but emits **102,310 single-edge
  commits** (96% of submits) — the measured MC.3 bottleneck.
- **JTAG CI: xlarge + extended timeout** (`82523a3`) — first tried pinning to
  self-hosted (`c3e9e0f`), but that serialised all Metal jobs on the one box
  (slower wall-clock). Reverted to the `macos-latest-xlarge` conditional (like
  the `metal` job) so JTAG runs in **parallel** on main, with step timeout
  15→40min / job 20→45min to fit xlarge's ~3× rate (validated: **32.0 min** on
  xlarge, 8 min margin). Self-hosted PR runs still ~10 min, well under.

## Done (1st session, all merged to `main` unless noted)

- **#108 reg_init value-injection** (PR #110, merged) — `reg_init` in
  testbench JSON, `$deposit` at t0. `src/testbench.rs`,
  `src/sim/trace_signals.rs` (`resolve_to_input_state_pos`),
  `src/sim/cosim_metal.rs`. **+ fixed zero-SRAM cosim panic** (nil
  `MTLBuffer`, sized `max(1)`).
- **#96 bidir tristate read-back** (PR #111, merged) — `Y = OE ? A : external`
  mux in `src/aig.rs` (`bi_24t` branch); ADR 0016 updated; truth-table test.
- **#95 closed** (was already done via `296cc12`). Cross-linked #102/#103/#106/#108.
- **CI / runners** (PR #112, merged): re-enabled **CUDA + HIP on the new
  GitHub-hosted T4 `tesla4-runner`** (every push); Metal light jobs offload to
  **`macos-latest-xlarge`** on `main`/`ci:metal-xl` label (gated, billed),
  else free self-hosted `macos-runner-1`; **`pull_request:` trigger
  un-filtered** (stacked PRs now get CI); workflow `concurrency` cancel.
  **Maintainer set a $200/mo Actions budget** — that's what un-stuck the T4
  (was `Shutdown` with no budget).
- **#113 cross-backend equivalence test** (merged) — `scripts/ci/
  compare_backend_vcds.py` + `backend-equivalence` job diffs CUDA/HIP/Metal
  output VCDs (functional + `--xprop`). **Verified bit-identical.**
- **#114 cosim portability ADR + plan** (merged) — ADR 0017 *Amendment
  2026-06-05*; `docs/plans/cosim-backend-portability.md` (#105 phasing).
- **#115 Phase 0 step 1** (OPEN) — consolidated cosim's ~265-line private
  `simulate_block_v1` duplicate onto `cpu_reference`; verified
  behaviour-preserving (release `--check-with-cpu` report identical).

## Carry-forward / open threads

- **v0.1.0 is pushed but NEVER TAGGED.** Release commit `7fed695` is on
  `main`; to finish: `git tag -a v0.1.0 -m "v0.1.0" <commit> && git push
  --tags` → `release.yml` draft → Homebrew tap PR (`gpu-eda/homebrew-tap`) →
  `netlist-graph` PyPI + tag. (Deliberately maintainer-triggered; see
  `docs/release-process.md`.)
- **#105 cosim portability — Phase 0 DONE** (branch `cosim-backend-seam-phase0`,
  commits `bda27ce` + `cf053b7`; not yet pushed). `CosimBackend` trait +
  `MetalBackend` extracted in-place in `cosim_metal.rs`, Metal bit-identical
  (harness + JTAG verified). **Next: Phase 1** (CpuBackend + Linux CI), then
  P2 (CUDA/HIP backend **with** Tier-2 GPU peripherals, T4-testable), P3
  (single-source peripherals). Authoritative design: ADR 0017 *Amendment
  2026-06-07* + `docs/plans/cosim-backend-portability.md`. Phase-1 entry
  points: implement `CpuBackend: CosimBackend` (the trait is at
  `cosim_metal.rs` ~`:2210`), reuse `cpu_reference::simulate_block_v1` for
  `run_edges` (N=1), add `state()`/`state_mut()` to the trait, de-Metal the
  VCD ring (see deferrals in Goal section). The `--check-with-cpu` per-block
  CPU stepper loop in `run_cosim` is a working prototype of the CpuBackend
  step path.
- **CDC/island batching (multi-clock plan MC.1→MC.4)** is the long-term fix
  for JTAG's single-edge tail — fast `sys_clk` island runs ahead/batched
  while only the model-driven `tck` boundary needs per-edge handover. MC.4
  needs the MC.1 island partitioner; **orthogonal to and larger than the #105
  seam.** Trigger now measured (#117).
- ~~**Latent debug-only bug** (mask bit-31 overflow)~~ — **DONE in #116**
  (open, green). `mask & mask.wrapping_neg()` in `cpu_reference.rs`.
- ~~**Verify the `macos-latest-xlarge` path schedules on a main push**~~ —
  **RESOLVED**: it does schedule, but is ~3× slower for heavy per-edge cosim
  → JTAG timed out at the 15-min step cap. Final fix (`82523a3`): JTAG stays
  on the xlarge conditional (parallel offload) with step/job timeouts raised
  to 40/45 min — validated at 32.0 min on xlarge. (A self-hosted pin was
  tried first but serialised the Metal jobs.) Re-evaluate after multi-clock
  batching cuts JTAG's per-edge tail (MC.3/MC.4).
- **#104** (CUDA/HIP `sim` timing) — Metal-only today; now T4-testable.
  **#106/#107** (x-assert detection/SVA). **#103** (multi-SRAM preload).
- **Single-source `simulate_block_v1` macro-prelude** (cross-shader compute
  kernel) — now safe to attempt because #113 guards it. Optional.

## Key decisions & findings

- **Batch utilisation measured (this session, #117).** GPU-peripheral cosim
  runs 100% batched; only CPU-side models (JTAG replay) + diagnostic modes
  (`--check-with-cpu`/`--trace-signals`/dff-dump/deep-diag) force `batch=1`.
  Drives the *batch-capable trait* refinement above. Telemetry now in the run
  summary (`Batch utilisation:` line). Reusable harness: `cosim_fixtures.sh`.
- **cosim is per-edge dispatch on every backend** (reactive path) → it
  sidesteps CUDA cooperative `grid.sync` (hardest-to-port, `sim`-only). But
  Metal's *production* path is batched (see batch refinement above) — the
  trait must accommodate both. This is why cosim portability is more tractable
  than `sim` but the seam is subtler than the original sketch.
- **Peripheral models are already backend-agnostic CPU Rust**
  (`src/sim/models/*.rs`); on-GPU IO kernels are a Metal perf optimization,
  not a correctness prerequisite. `cpu_reference::simulate_block_v1` is the
  CPU design stepper; the `--check-with-cpu` path is a working prototype.
- **Backends are bit-identical** on `dff_test` (functional + xprop) —
  proven against real CI artifacts. CUDA/HIP code did NOT bitrot despite no CI
  since May.
- **Cross-shader tools rejected (for now):** Ferrox = no HIP, v0.1, ML-shaped.
  Slang = mature but AMD via Vulkan (not ROCm/HIP), no grid.sync, full
  rewrite. Neither closes the two real gaps. (Slang issue #9592 the maintainer
  spotted is *cooperative matrix*/tensor-core, unrelated to *cooperative
  groups* grid.sync.) Decision: in-house macro-prelude + equivalence test.
- **Stacked-PR + rebase-merge gotcha:** each merge gives new SHAs, so the next
  PR up owes a `git rebase origin/main` (patch-id-skips the dupes). Cost is
  inherent to rebase-merging a stack.

## `--check-with-cpu` mismatch (NOT a bug)

cosim `--check-with-cpu` reports a persistent INPUT mismatch at `word[0]`
bits [0,1] (clock/posedge flags) + one downstream output — present **before
and after** #115 (verified by stashing). It's a pre-existing modelling
artifact (GPU `state_prep` clock-flag injection vs the CPU check path), not a
real divergence and not in scope.

## References

- cosim portability: ADR 0017 (Amendment 2026-06-07 — target architecture),
  `docs/plans/cosim-backend-portability.md`, issue #105.
- Equivalence guard: `scripts/ci/compare_backend_vcds.py`, `backend-equivalence`
  CI job, PR #113.
- Runners: `.github/workflows/ci.yml`, `.github/actionlint.yaml`
  (`tesla4-runner` registered), `ci:metal-xl` label.
- Release: `docs/release-process.md`, ADR 0018, `packaging/README.md`.
