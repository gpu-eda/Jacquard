# Handoff — `sim` on ROCm: no cooperative launch

## Goal & next-up

**Goal:** make `jacquard sim` work on AMD/ROCm, which it has never done. `sim`
dies at `csrc/kernel_v1.hip.cpp:65` (`hipLaunchCooperativeKernel`) with
`unspecified launch failure` on the simplest design that exists (1 block,
1 stage, 6 cycles). The AMD runner's GPU **does not support cooperative
launch** — measured, not inferred (see Critical context).

**Very next action — do this first, it is a correctness fix to landed docs:**
two claims now on `main` are **backwards** and were written by this session.
The runner is a **Raphael APU iGPU, gfx1036**, reporting as `gfx1030` because
of an `HSA_OVERRIDE_GFX_VERSION` spoof.

1. `.github/actionlint.yaml` says *"The `gfx1036` label is wrong: the hardware
   reports **gfx1030**"*. Invert it: the label is right, the report is spoofed.
2. `docs/spikes/amd-laptop-backend.md` (~line 26) says *"it reports gfx1030,
   despite a gfx1036 runner label … **Green HIP CI on that runner would tell us
   nothing about an AMD laptop.** That is the finding that started this spike."*
   That premise is wrong, and wrong **in our favour**: the runner is an
   *integrated* RDNA2 GPU, so the 14/14 ROCm cosim goldens are already passing
   on an APU — a far better laptop proxy than a discrete card. Also fix
   "Next steps" item 4 ("Fix the runner label"), which is now backwards.
3. `.github/workflows/ci.yml` (`hip-on-rocm` job comment) repeats the gfx1030
   claim and says "no HSA_OVERRIDE needed" — inherited from the spike doc and
   likely false. Confirm whether the override is set in the runner image
   (`env | grep -i hsa`, `rocminfo`) before rewording.

**Then:** build the non-cooperative `sim` fallback. Design is decided (below).

## Done this session

Landed on `main` via **PR #209** (merged, all 26 checks green, #203 closed):

- `4215ba9e` `fix(kernel): staged-IO reads land 2^32 words out of bounds` — the
  #203 fix. Signed `1 << 31` is `INT_MIN`, so the staged-IO base-pointer bias
  went **+**2^31 instead of −2^31 and `[idx]` added another 2^31 → every staged
  read landed 2^32 words (16 GiB) past the buffer. Only reachable with
  `--level-split` (single-stage designs have an empty `staged_io_map`) and only
  from stage 1 on. **nvcc narrowed the address arithmetic to 32 bits, which
  truncated the overflow back to the intended offset — so CUDA was silently
  *correct*, not silently wrong.** ROCm computes it exactly and faulted. Fixed
  by clearing the flag from the *index* (the `cpu_reference.rs:58` idiom), which
  removes the UB rather than correcting its sign.
- `ef7beda8` `fix(metal)` — same idiom in `kernel_v1.metal`; Metal was already
  correct (`1u << 31`) but formed the same OOB pointer.
- `92a071e2` `ci: promote the ROCm probe to a real job` — `HIP Tests (ROCm
  backend)` in `ci.yml`, permanent + gated. Deleted `hip-laptop-arch-probe.yml`.
- `001c376b` `docs(spike)` — spike resolution.
- ADR 0015 §5 now documents the bit-31 flag as a cross-backend wire format with
  one encoder and four hand-written decoders.

Filed: **#217** — cosim `--output-vcd` + any timing source auto-enables arrivals
and panics on CUDA/HIP/CPU (Metal-only guard). Code trace, not executed.

In flight: **PR #220** (draft, branch `ci/rocm-full-gpu-suite`), commits
`1456fc29` + `08f14e3f`. **Currently red by design** — it points the ROCm job at
`gpu_test_suite.sh` (which reaches `sim`) and adds an inline cooperative-launch
probe. It has served its purpose; see follow-up 4.

## Open follow-ups

1. **Correct the false docs on `main`** — see "Very next action".
2. **Non-cooperative `sim` fallback** (decided by Rob: build it now, don't just
   gate). Design, in preference order:
   - **Generalise `cosim_simulate_stage` → `simulate_v1_stage`** in
     `kernel_v1_impl.cuh:713`, adding a `u32 cycle_i` param and computing
     `input = base + cycle_i*state_size`, `output = base + (cycle_i+1)*state_size`.
     This is **exactly what `kernel_v1.metal:625-626` already does**.
   - Cosim then calls it with `cycle_i = 0` — behaviour unchanged *by
     construction*, which is how you keep the 14/14 goldens honest while
     touching a kernel four backends depend on.
   - `sim` fallback = host loop over cycles×stages, mirroring `sim_metal`
     (`src/bin/jacquard.rs:1161`). Each launch is the barrier.
   - **Collapse the two HIP launchers** (`kernel_v1.hip.cpp:33` untimed and
     `:111` timed). They are near-identical — same kernel, same `arg_ptrs[12]`,
     differing only in `nullptr` vs real timing args. One helper means the
     coop-vs-fallback decision lives in one place, not four.
   - Branch on `hipDeviceGetAttribute(hipDeviceAttributeCooperativeLaunch)`.
     Keep the cooperative scan as the fast path where supported. We currently
     query `hipDeviceAttributeWarpSize` and *never* CooperativeLaunch — we just
     launch and hope, which is why the error is `unspecified launch failure`
     instead of a sentence.
   - Prefer doing the branch **inside the C launcher**: the signature already
     takes `num_cycles`, so the Rust side need not change at all.
3. **Ship the cooperative-launch check in `scripts/amd-laptop-probe.sh`**
   (decided by Rob). That script already asks volunteers with AMD laptops to run
   it and paste a report; ~10 lines answers "does `sim` work on
   gfx1103/1150/1151?" from people who own the silicon. We cannot answer it
   ourselves — **we have measured exactly one AMD device, and it's spoofed.**
4. **Resolve PR #220.** Either revert the `gpu_test_suite.sh` step to cosim-only
   (safe; it's red until the fallback exists) or hold it as the test that proves
   the fallback. Move the inline probe out of `ci.yml` — it belongs in
   `amd-laptop-probe.sh` (3) and/or the debug workflow (5).
5. **Dispatchable ROCm-only debug workflow** (Rob asked; NOT built). Iterating
   via `ci.yml` burns macOS + CUDA runners for a ROCm question. Wants
   `workflow_dispatch` + push on the debug branch, `[self-hosted, amd, rocm]`
   only. Precedent: the deleted `hip-laptop-arch-probe.yml`. **Do not "comment
   out cuda/metal in ci.yml"** as a shortcut — that edit can land and silently
   gut CI.
6. **#172 (CI build/test split) will silently delete the ROCm job.** It is a
   **draft, 63 commits behind main**, and rewrites `ci.yml` wholesale
   (2056 → 129 lines) from a base that predates `hip-on-rocm`. A careless
   rebase resolution drops the job and *nothing fails to tell you*. Its own
   follow-up list item 4 is "Real AMD/ROCm runner" — #209 already did that, so
   the PR is stale in exactly this area. Its `jacquard-hip` artifact is
   `HIP_PLATFORM=nvidia` and **cannot execute on AMD**, so ROCm needs its own
   build config or build-in-place. Its "green on first run" validation is 63
   commits old.
7. **#217** — the cosim timing trap.
8. **`docs/timing-simulation.md`** — the table is accurate, but every "✅ HIP" in
   it means *HIP-over-CUDA*. Needs a footnote distinguishing that from real
   ROCm, where `sim` does not run at all. This is the ambiguity the whole spike
   existed to expose.

## Critical context

**The probe result** (`ci.yml` `hip-on-rocm`, run 29468955058) — this is the
datum everything rests on:

```
device      : AMD Ryzen 5 7600 6-Core Processor   <-- a CPU's name = an APU iGPU
gcnArchName : gfx1030                             <-- spoofed; hardware is gfx1036
CUs         : 1   warpSize: 32                    <-- a real gfx1030 has 36-80 CUs
prop.cooperativeLaunch            = 0
attr CooperativeLaunch            = 0
maxActiveBlocksPerCU(coop_kernel) = 8  → coop grid cap = 8

launches:
  plain, 1 block               launch=no error                     sync=no error
  cooperative, 1 block         launch=unspecified launch failure   sync=no error
  cooperative, 2 blocks        launch=unspecified launch failure   sync=no error
```

A *trivial* `grid.sync` kernel fails at 1 block while a plain launch of the same
shape succeeds → **the mechanism is unsupported; it is not our kernel, our
launch config, or occupancy.**

- **Do not generalise this to "RDNA2 can't do cooperative launch."** We measured
  one spoofed 1-CU APU. Discrete gfx1030 may well report `1`. That's what
  follow-up 3 is for.
- **`sim` on AMD may always have been fiction.** The spike doc argues *against*
  an OpenCL/Vulkan port partly on "cosim-only is the realistic scope, `sim`
  stays on CUDA/HIP". On APU-class AMD we are cosim-only *today* — the exact
  limitation the doc used to reject porting. This doesn't overturn the
  stay-on-HIP conclusion (226 lines vs a 1400-line kernel), but it sharpens it.
- **Metal is the reference design, not a special case.** It has no device-wide
  barrier, so it was *forced* into the general shape: one `simulate_v1_stage`
  serving both `sim` (`jacquard.rs:1043`) and cosim (`metal.rs:209`),
  parameterised by `current_cycle`/`current_stage`. CUDA/HIP carry two kernels
  doing nearly the same job. Converging on Metal's design is what makes the
  fallback ~20 lines instead of a new kernel.
- **This is not a #203 regression.** `sim` has never run on ROCm. The #203 fix
  only touches the `idx >> 31` staged-IO branch, which a single-stage design
  never enters, and CUDA runs the same suite green on that code.

**Environment gotchas:**

- **You cannot compile the HIP/CUDA path on macOS.** `csrc/kernel_v1_impl.cuh`
  and `kernel_v1.hip.cpp` are only built with `--features cuda|hip`. CI is the
  only check. Metal *can* be verified locally (see Verification) and the `.cuh`
  is shared with CUDA, so a `.cuh` change still needs a CI round trip.
- **`gh run view --log` returns empty for cancelled runs.** Use
  `gh api /repos/gpu-eda/Jacquard/actions/jobs/<job_id>/logs`, which works while
  the run is still finalising.
- **`ci.yml` only triggers on `push` to `main`/`staged-aig-release`** plus
  `pull_request`. Pushing a debug branch alone runs nothing; you need the PR
  (which is why follow-up 5 matters).
- **`rocgdb` hangs indefinitely** even with `--batch -nx` and stdin closed
  (killed by `timeout`, exit 124, no backtrace). `AMD_SERIALIZE_KERNEL=3
  AMD_LOG_LEVEL=3` is the attribution route that works — it's what pinned #203.
- **`gpu_test_suite.sh`'s header claims `jq` is required. It isn't** — nothing
  in it uses `jq`. All its fixtures are committed (incl. the 19.6 MB
  `tests/mcu_soc/data/6_final.v`); it needs no submodules beyond the two the
  ROCm job already inits. So a failure there is a real failure.
- Local branch `spike/amd-laptop-backend` is **stale** after PR #209's
  server-side rebase; it's merged. `ci/rocm-full-gpu-suite` is the live branch.

## Verification

Confirm the tree is as described:

```bash
# #203 fix is on main, both kernels on the safe idiom (expect: no matches)
git -C . grep -n "state - (1u\? << 31)" csrc/

# The ROCm job exists and is cosim-only on main / full-suite on the PR branch
git show origin/main:.github/workflows/ci.yml | grep -n "hip-on-rocm" 

# Metal still 14/14 locally (the only backend you can check without CI, ~1 min)
cargo build -r --features metal --bin jacquard
JACQUARD_BIN=target/release/jacquard COSIM_SCOPE=all bash scripts/ci/cosim_cpu_check.sh
# expect: === all 14 cosim fixtures PASS ===

# The ROCm failure, reproduced (needs the AMD runner via PR #220's CI):
#   jacquard sim tests/timing_test/dff_test_synth.gv tests/timing_test/dff_test.vcd out.vcd 1
#   → HIP error at csrc/kernel_v1.hip.cpp 65: unspecified launch failure
```

State of play: #209 merged, #203 closed, main green. #217 open. PR #220 draft +
red by design. #172 draft, stale, and hazardous to the ROCm job.
