# Stage C (T2.2) port spec — CUDA/HIP flash kernels + (optional) CpuBackend oracle

**Status:** ✅ **DONE** (C1–C4 complete, merged in PR #120, 2026-06-21,
T4-green). Plan A was taken (CpuBackend oracle first). Sibling to
[`cosim-phase2-cuda-hip.md`](cosim-phase2-cuda-hip.md) (Stage C = checkpoint
**T2.2**) and [`cosim-phase2-stageB-io-port.md`](cosim-phase2-stageB-io-port.md).
The C2/C3/C4 sections below are kept as the as-built record. Performance tuning
of the managed-memory backend is a separate follow-up (issue #122).

**Goal:** bring CUDA/HIP cosim to flash parity — the last stage of the CUDA/HIP
cosim track. Port `gpu_apply_flash_din` + `gpu_flash_model_step`, wire into the
batched `run_edges`, gate against a golden.

## Done (all merged in PR #120; main SHAs)

- **C1:** lifted `FlashState`/`FlashDinParams`/`FlashModelParams` from `metal.rs`
  to the shared `mod.rs` GPU-struct region (gated, `size_of` asserts 48/24/32).
  Metal bit-identical.
- **C2 (`CpuBackend` oracle):** wired `CppSpiFlash` into `CpuBackend::run_edges`
  (per-edge `apply_flash_din` + dual-step `flash_model_step`); patched
  `spiflash_model.cc` `p_d_i` init 0→0x0F to match the GPU `FlashState.d_i`. A
  10k-edge `mcu_soc` cosim on the no-GPU `CpuBackend` is byte-identical to the
  Metal GPU flash kernel → C++↔shader FSM equivalence proven, CpuBackend is a
  valid golden source. (Detail: the C2 section below.)
- **C3 (`a071326`):** ported `flash_eval_commit_persistent` + `gpu_apply_flash_din`
  + `gpu_flash_model_step` to `kernel_v1_impl.cuh` + `_cuda`/`_hip` launchers;
  wired into `Cuda/HipBackend::run_edges` (Metal order); flash buffers built in
  `new` via shared `build_flash_buffers_dev`; stubs replaced with real
  `FlashState` reads. `FlashState` byte offsets derived via `std::mem::offset_of!`
  (ABI drift → compile error). T4-green.
- **C4 (`7c54448`):** flash CI gate — 74 KB `CpuBackend` golden
  `tests/mcu_soc/expected/mcu_flash.vcd` (== Metal, deterministic), self-contained
  `tests/mcu_soc/sim_config_selfcontained.json`, `COSIM_SCOPE=flash` in
  `cosim_cpu_check.sh`, wired into Linux + CUDA/HIP T4 jobs. All three flash-gate
  steps T4-green (byte-identical vs golden).

## Scoping findings (the part that changes the plan)

1. **No pure-CPU flash stepper exists.** `CppSpiFlash` (C++ FFI SPI/QSPI model,
   `testbench.rs`; `step(clk,csn,d_o)->d_i`) is instantiated in `run_cosim_generic`
   (`mod.rs:1965`, loads the 16 MiB firmware) but is **`_`-prefixed and never
   stepped**. The `--check-with-cpu` path injects `backend.flash_d_i()` (the GPU's
   value) — it does not run a CPU flash model. So `CpuBackend` flash is a *stub*
   (`flash_d_i → 0x0F`), and wiring a real CPU flash path is **from-scratch work**,
   not "connect the existing model."
2. **`CppSpiFlash` has no reset API** — reset handling (the GPU `FlashState.in_reset`)
   has no CPU analog; would need to idle the model (step with csn high) during reset.
3. **C++ `CppSpiFlash` FSM vs GPU `gpu_flash_model_step` shader FSM equivalence is
   unvalidated.** They are *meant* to be the same SPI FSM, but byte-identical d_i
   sequences across all commands/edge cases were never proven. A shared CPU/GPU
   golden depends on this.
4. **Fixture: `mcu_soc` is the only flash cosim fixture, and it is committed +
   self-contained** — `tests/mcu_soc/data/6_final.v` (19.6 MB netlist) and
   `tests/mcu_soc/software.bin` (3.6 KB firmware) are both git-tracked, so it runs
   on a fresh checkout without the `chipflow` firmware build. But it is heavy
   (whole SoC; full boot is 500K edges). A *short* run (a few k edges) exercises the
   deterministic boot-time flash command/address/read sequence — enough for a
   regression golden (flash-pin VCD or decoded flash transactions).

## The decision this raises

Option (a) — *"wire `CppSpiFlash` into `CpuBackend`"* (chosen before finding #1–3) —
is now known to be a from-scratch CPU flash stepper **plus** an FSM-equivalence
proof, and it is **orthogonal to the release goal**: CUDA/HIP parity needs the GPU
flash kernels gated against a golden, and Metal already produces a correct one.

Two viable plans:

- **Plan B-first (recommended): GPU kernels gated vs the Metal `mcu_soc` golden.**
  Do C3 (port the two flash kernels to CUDA/HIP + wire into `run_edges`) and C4
  (short `mcu_soc` cosim in CI; CUDA/HIP/Metal VCDs compared via the existing
  `backend-equivalence` harness, or vs a committed Metal golden). This is the direct
  release-critical path. The `CpuBackend` flash oracle becomes an **optional
  follow-up** (own effort, with the FSM-equivalence validation built in).
- **Plan A (as originally chosen): CpuBackend flash oracle first.** Implement the
  CPU flash stepper (C2), prove it byte-identical to Metal on a short `mcu_soc`
  run, commit that as the no-GPU golden, then C3/C4 gate CUDA/HIP against it.
  Completes the Tier-1-oracle story + Linux no-GPU flash coverage, but is larger and
  carries the equivalence risk on the critical path.

## C2 — CpuBackend flash stepper: the exact dual-step convention (chosen plan a)

This is the make-or-break detail. `CppSpiFlash::step(clk, csn, d_o)` is a **single**
eval()+commit() returning `p_d_i` (CXXRTL `agent.step()` semantics,
`spiflash_model.cc:165`). The GPU `gpu_flash_model_step` is a *direct port* of the
same `spiflash_model.cc`, but per call it does a **dual-step with delayed CSN**
(shader :1008-1018), and is called **once per edge** (2×/tick):

```
// step 1: delayed csn + delayed d_out (processes the clock edge, samples old data)
flash_eval_commit(clk, prev_csn, prev_d_out)
// step 2: delayed csn + current d_out
d_i = flash_eval_commit(clk, prev_csn, d_out)
// then store for next edge:
prev_csn   = csn      // current OUTPUT csn → next edge's delayed csn
prev_d_out = d_out
```

where `clk`/`csn`/`d_out` are read from the **output** state slot
(`clk_out_pos`/`csn_out_pos`/`d_out_pos[4]`), and `prev_csn`/`prev_d_out` are the
*previous* edge's output values (the setup-delay model). `model_prev_csn` (the
model's internal `prev_csn_o` edge-detect state) is threaded through both evals;
because step 2 re-feeds the same `prev_csn`, no spurious CSN edge is seen between
the two evals — `CppSpiFlash`'s own `commit()` (`prev_csn_o = csn`) reproduces this
automatically when called as above. Reset branch (shader :974-980): force
`d_i = 0x0F`, set `prev_csn=csn`, `prev_d_out=d_out`, **do not step** the model.

**CpuBackend wiring** (`mod.rs`): add fields `flash: UnsafeCell<CppSpiFlash>`,
`flash_clk/csn_out_pos`, `flash_d_out_pos[4]`, `flash_d_in_pos[4]`, `flash_xmask_off`,
`flash_d_i`, `flash_prev_csn`, `flash_prev_d_out`, `flash_in_reset`. In `new`, build
`CppSpiFlash` from `config.flash` (load firmware) + resolve positions (the
orchestration already computes these as locals at `mod.rs:1812-1860` — mirror it).
In `run_edges`, per edge: **apply_flash_din** (inject `self.flash_d_i` into input
MISO `d_in_pos`, clear X-mask) *before* `simulate_block_v1`; **flash_model_step**
(the dual-step above) *after*. `flash_d_i()` returns `self.flash_d_i` (drop the
`0x0F` stub); `flash_set_in_reset` stores the flag.

**Init-state equivalence hazard (confirmed).** The GPU inits `FlashState`
(`build_flash_buffers`): `data_width=1, prev_csn=1, model_prev_csn=1, d_i=0x0F,
in_reset=1`, rest 0. The C++ `SpiFlashModel` constructs `State{data_width=1,…0}`
(matches) **but** `p_d_i = 0` (`spiflash_model.cc:27`) vs the GPU's `d_i = 0x0F`.
`d_i`/`p_d_i` is a *persistent* output (only written on negedge_clk during a read
command's data phase), so during command/address phases `CppSpiFlash` would carry
MISO=0 while the GPU carries 0x0F. The design doesn't sample MISO outside the
read-data phase (functionally harmless), but a raw flash-pin VCD diff would flag
it. **Fix:** change `spiflash_model.cc:27` to `uint8_t p_d_i = 0x0F;` to match the
GPU init — safe, `CppSpiFlash` has no live callers (verify with a repo-wide grep
first). The reset branch already forces `flash_d_i = 0x0F` without stepping, so
the pre-reset-release values align once this init is fixed. Also force the CPU
reset behaviour to mirror the shader (`d_i=0x0F`, set prev_csn/prev_d_out, do not
step) so `CppSpiFlash`'s internal `prev_csn_o` stays high through reset (csn idle
high in `mcu_soc`).

**Validation (C2 — DONE, byte-identical):** reproducible recipe (the `/tmp` config
is ephemeral — regenerate it):
```bash
python3 -c "import json,pathlib; c=json.loads(pathlib.Path('tests/mcu_soc/sim_config.json').read_text()); c['flash']['firmware']='tests/mcu_soc/software.bin'; pathlib.Path('/tmp/mcu_sc.json').write_text(json.dumps(c,indent=2))"
cargo build -r --bin jacquard                      # no-feature → CpuBackend
cargo build -r --features metal --bin jacquard
for b in cpu metal; do f=$([ $b = metal ] && echo '--features metal'); \
  ./target/release/jacquard cosim tests/mcu_soc/data/6_final.v --config /tmp/mcu_sc.json \
  --top-module top --max-clock-edges 10000 --output-vcd /tmp/mcu_$b.vcd; done
diff /tmp/mcu_cpu.vcd /tmp/mcu_metal.vcd            # byte-identical ⇒ FSMs match
```
~40s partitioning + ~4s sim per run. Flash is actively read by ~10k edges
(`cmd 0x03 @ 0x100000`). This same comparison is the C3 gate for CUDA/HIP (vs the
committed CpuBackend golden) and the C4 CI gate. (If a future port diverges, the
C++↔shader bug is in `flash_eval_commit_persistent` vs `eval()`/`commit()`.)

## C3 — CUDA/HIP flash kernels (mirror Stage B mechanics)

- Port `gpu_apply_flash_din` (shader:904 — write `FlashState.d_i` → input-state MISO
  bits at `d_in_pos`, clear their X-mask) and `gpu_flash_model_step` (shader:943 —
  read clk/csn/d_out from output state, dual-step the SPI/QSPI FSM, update
  `FlashState`) to `kernel_v1_impl.cuh` + `extern "C"` `_cuda`/`_hip` launchers.
  **Also port the `flash_eval_commit_persistent` helper (shader:843)** — the
  per-eval primitive `gpu_flash_model_step` calls twice (it is the shader port of
  `spiflash_model.cc` `eval()`+`commit()`); it must live in the `.cuh` too.
  Needs the 16 MiB firmware `UVec<u8>` (load from `config.flash.firmware` like
  `CpuBackend::new` / `build_flash_buffers`'s `flash_data_buffer`) + persistent
  `FlashState` (`UVec<u8>`, the event-buffer FFI pattern; init per
  `build_flash_buffers`: `data_width=1, prev_csn=1, model_prev_csn=1, d_i=0x0F,
  in_reset=1`). Structs already shared (C1). The launchers cross the struct/firmware
  buffers as `u8*` and cast (Stage B convention).
- Wire into `CudaBackend`/`HipBackend::run_edges` in the Metal order: `state_prep` →
  **`gpu_apply_flash_din`** → `simulate_stage × N` → **`gpu_flash_model_step`** →
  `gpu_io_step` → snapshot. Build the flash buffers in `new` (mirror
  `MetalBackend::build_flash_buffers`); replace the Stage-B `flash_d_i` /
  `flash_debug_snapshot` stubs with GPU `FlashState` reads; `flash_set_in_reset`
  drives `FlashState.in_reset`. Model-driven-clock (JTAG) runs the peripherals at
  `batch=1` within the same backend.

## C4 — CI gate

Add a short `mcu_soc` (and/or `jtag_minimal`) flash cosim to the GPU CI jobs;
compare CUDA/HIP against the golden (Metal, or CpuBackend under Plan A). Note the
runtime cost of the 19.6 MB netlist on the runners; keep edge counts small.

## Risks

- **FSM equivalence** (C++ `CppSpiFlash` ↔ shader `gpu_flash_model_step`) — only
  matters under Plan A (shared CPU golden); Plan B sidesteps it (Metal is the GPU
  reference).
- **CI-only GPU validation** on the T4, as in Stage B.
- **Fixture weight** — `mcu_soc` is large; a short run must still be deterministic
  and exercise real flash transactions.
