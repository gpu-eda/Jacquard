# Spike — reaching AMD laptops

**Status:** The compute-API question is **answered — stay on HIP, do not port
to OpenCL** (see "Revised conclusion"). ROCm now builds, runs, and passes all 14
**cosim** goldens byte-identically, guarded by the permanent `HIP Tests (ROCm
backend)` CI job — and on APU-class silicon, which is better news than this doc
originally thought (see the correction below).

`sim` now runs on ROCm too — it never had, because the device has no
cooperative launch — via a non-cooperative fallback; see "`sim` on a device
without cooperative launch" below.

What remains open is the last mile: whether the laptop archs
(`gfx1103`/`gfx1150`/`gfx1151`) *run* correctly, which needs silicon we don't
have.

**Question:** what does Jacquard need in order to run on an AMD laptop, and is
the answer HIP/ROCm, OpenCL, Vulkan, or something else?

## Why this is a question at all

The `HIP Tests (NVIDIA backend)` job builds `hip-runtime-nvidia` and runs on
`tesla4-runner`: HIP-over-CUDA, never ROCm. The org's self-hosted AMD runner sat
online and idle because nothing targeted it (#198).

Probing it (2026-07-15) showed the box itself is fine — ROCm 7.2.4, `hipcc` (HIP
7.2.53211), `hipconfig --platform` = `amd`, and a trivial HIP kernel compiles and
runs correctly on the GPU:

```
--- native ---
result: 1 2 3 4 5 6 7 8
NATIVE: OK
```

It reports **`gfx1030`**, despite a `gfx1036` runner label — and the original
version of this spike drew the wrong conclusion from that, which is worth
recording because the error ran for a while and pointed the whole investigation
the wrong way.

**Corrected (2026-07-16, measured on the runner):** the label is right and the
*report* is spoofed. The runner is the **Raphael iGPU integrated into nvidia1's
own Ryzen 5 7600** — `lspci` says `1002:164e` (Raphael), which is `gfx1036`, and
the `amd-runner` container image bakes in `HSA_OVERRIDE_GFX_VERSION=10.3.0`:

```
$ docker inspect amd-runner --format '{{range .Config.Env}}{{println .}}{{end}}'
HSA_OVERRIDE_GFX_VERSION=10.3.0
RUNNER_LABELS=self-hosted,amd,gfx1036,rocm,hip,vulkan,cubecl

$ rocminfo | grep gfx                        →  gfx1030    # spoofed
$ env -u HSA_OVERRIDE_GFX_VERSION rocminfo   →  gfx1036    # the truth
```

So the claim that *"green HIP CI on that runner would tell us nothing about an
AMD laptop"* was **backwards, and wrong in our favour.** The runner is an
*integrated* RDNA2 GPU — an APU, on the same support tier as the laptop parts we
care about, not a discrete board on ROCm's main matrix. The 14/14 ROCm cosim
goldens are therefore *already* passing on APU-class silicon. That is a far
better laptop proxy than a discrete gfx1030 would have been.

Note what that also means: `gfx1036` (Raphael) appears nowhere on ROCm's
supported lists (see the table below) — and it works anyway, via the override.
That is a data point about how binding those lists really are.

## What ROCm actually supports on laptops (as of 2026-07-15)

Prior assumption — "ROCm doesn't do laptop APUs, you need
`HSA_OVERRIDE_GFX_VERSION`" — is **out of date**. ROCm 7.2.1 added official
Ryzen APU support. But it's narrower than "AMD laptops":

The [main compute compatibility matrix][matrix] lists **no APU targets at all**
(`gfx908, gfx90a, gfx942, gfx1030, gfx1100, gfx1101, gfx1200, gfx1201, gfx950`).
APU support lives in a separate tier, [Use ROCm on Radeon and Ryzen][radeon].
The [native-Linux support matrix][ryzlinux] there lists exactly **two** gfx
targets:

| Silicon | Target | Parts |
| --- | --- | --- |
| Strix Halo | `gfx1151` | Ryzen AI Max+ 395, Max 390, Max 385 |
| Strix Point | `gfx1150` | Ryzen AI 9 HX 375, HX 370, 365 |
| 400-series | `gfx1150`/`gfx1151` | Ryzen AI 9 HX 475, HX 470, 465 |

Not listed: **`gfx1103`** (Phoenix / Hawk Point — Ryzen 7040/8040), `gfx1035`
(Rembrandt), `gfx1036` (Raphael), `gfx90c` (Cezanne). One search summary claimed
`gfx1103` was supported; the authoritative page does not list it. Treat as
unconfirmed pending the research below.

Support is also version-churny — [ROCm#5339][5339] is titled "Confusing rocm
support for gfx1151", and reports suggest 6.4.2 had `gfx1151` but not `gfx1150`,
with both in the 7.13.0 preview matrix.

### The install contract is the real problem

Per the [Ryzen Linux install guide][ryzinstall], running ROCm on a supported APU
needs:

- **Ubuntu 24.04.4** specifically (24.04.3 "preliminary");
- the **`6.14-1018` OEM kernel or newer** (`apt install linux-oem-24.04c`);
- `amdgpu-install -y --usecase=rocm --no-dkms` — `--no-dkms` is *mandatory*
  (inbox drivers required); if DKMS lands anyway, `autoremove amdgpu-dkms dkms`;
- **BIOS changes**: minimum dedicated VRAM (0.5 GB) plus a raised TTM limit (via
  `amd-ttm` from the `amd-debug-tools` PyPI package);
- `usermod -a -G render,video` + reboot;
- no in-place upgrades — uninstall before upgrading.

And a trap that presents as "the tool is broken": on `gfx1150`, [GPU detection
fails when UMA is "Auto"/Dynamic VRAM][ollama11451] and silently falls back to
CPU. Fixed VRAM in BIOS works.

So even on *supported* laptop silicon, ROCm is: two gfx targets, one Ubuntu
point release, a specific OEM kernel, and a BIOS change. That is a demanding
contract to put in front of someone who just wants to simulate a netlist.

## What a non-CUDA backend costs us

Measured, not estimated:

| File | Lines | Role |
| --- | --- | --- |
| `csrc/kernel_v1_impl.cuh` | **1462** | the kernel logic |
| `csrc/kernel_v1.cu` | 207 | CUDA launch wrapper — `#include`s the impl |
| `csrc/kernel_v1.hip.cpp` | **226** | HIP launch wrapper — **`#include`s the same impl** |
| `csrc/kernel_v1.metal` | **1441** | can't share it; full reimplementation |

Host side mirrors this: `cuda.rs` 690, `hip.rs` 695, **`metal.rs` 2116**.

**AMD support currently costs ~226 lines because HIP is source-compatible with
CUDA.** Metal is the honest precedent for a backend that isn't: a whole parallel
kernel plus 3× the host code. Any move off HIP moves AMD from the first column
to the second.

### The blocker: `sim` needs a device-wide barrier

`kernel_v1_impl.cuh:623` calls `cooperative_groups::this_grid().sync()` — a
grid-wide barrier *inside* the kernel, via `hipLaunchCooperativeKernel`.
**Neither OpenCL nor Vulkan has a device-wide barrier primitive.**

`cosim` does not have this problem, and says so itself:

> Unlike the `sim` scan above, cosim is reactive (inputs depend on outputs), so
> the host drives one scheduler edge at a time over a 2-slot [input|output]
> state. These kernels are NON-cooperative ordinary launches — the host loops
> major stages and each launch is the grid-wide barrier — so cosim never needs
> the cooperative grid.sync the scan relies on. They mirror Metal's `state_prep`
> and `simulate_v1_stage`.

So the realistic scope of any portable-compute backend is **cosim only, `sim`
stays on CUDA/HIP** — unless `sim` is restructured into N host-driven launches,
which is a barrier per sync point and is precisely what the cooperative launch
exists to avoid.

### What makes it cheaper than feared

- **Zero templates** in the impl; only 26 CUDA qualifiers
  (`__device__`/`__global__`/`__shared__`/`__forceinline__`). It's C-like CUDA,
  so a port is transliteration plus the sync problem, not a fight with a type
  system.
- **The cross-backend goldens already exist** — CpuBackend == Metal == CUDA ==
  HIP, byte-identical. A new backend gets a correctness oracle on day one. The
  timestamp maths is host-side Rust (#195), so goldens should match outright.

### The recurring cost

Two kernel implementations become three. Every kernel-level change lands three
times and must stay byte-identical against the goldens. That tax is forever and
is larger than the port. Plus: a `Device::OpenCL` variant in vendored `ulib`
(the `[CPU] [CUDA] [HIP] [Metal]` device-ID layout is positional — additive, but
a submodule change), 24 `CosimBackend` methods, and a packaging change (OpenCL
compiles kernels at runtime from source/SPIR-V, unlike the build-time `ucc`
compile).

## What the local-LLM community has learned — and why most of it isn't our problem

llama.cpp / ollama have driven these parts in anger far longer than any vendor
matrix reflects. Their pain on AMD laptops is real and well documented. It is
also, **almost entirely, rocBLAS pain** — which we don't have.

**gfx1103 (Radeon 780M)**, [llama.cpp#20839][20839] — three failure modes:

1. Flash-Attention **WMMA** kernel: "no device code compatible with HIP arch
   1300", tuned for discrete RDNA3;
2. **rocBLAS TensileLibrary** missing: "Cannot read TensileLibrary.dat … for GPU
   arch: gfx1103" — ROCm 6.3.2 ships gfx1100/1101/1102 only;
3. **MMQ** kernels: `HSA_OVERRIDE_GFX_VERSION=11.0.0` spoofing gives "invalid
   device function".

The decisive line in that issue: *"The problem didn't exist in older llama.cpp
versions (~late 2024 vintage) that **embedded HIP kernels directly rather than
calling rocBLAS externally**."* Vulkan works there, ~2 s/generation slower than
a working ROCm build.

**gfx1151 (Strix Halo)**, [llama.cpp#13565][13565] — an *officially supported*
part where HIP is 2.5× slower than Vulkan (pp512: HIP 348 tok/s vs Vulkan 881).
Tellingly, compiling for gfx1100 and spoofing `HSA_OVERRIDE_GFX_VERSION=11.0.0`
reaches ~599 — **faster than the native gfx1151 path**. Both hit max clock, so
it isn't hardware; it's untuned rocBLAS/Tensile kernels for the arch. Still open.
See also [ROCm#5643][5643] (hipBLASLt falls back on gfx1151 as unsupported) and
the community's [custom rocBLAS builds for gfx1103][rocmlibs] — an entire cottage
industry of rebuilding *the library* per arch.

### Why this mostly doesn't bind on us

Checked against our kernel:

- **No BLAS.** Zero references to `rocblas`/`hipblas`/`cublas`/`tensile`
  anywhere in `csrc/` or `src/`. Failure modes 2 and 3, and the gfx1151
  performance gap, are all rocBLAS/Tensile artefacts.
- **No matrix intrinsics.** No `wmma`/`mfma`/`matrix_core`. Failure mode 1 is a
  WMMA kernel. We simulate AND gates.
- **One arch-specific intrinsic**, `__shfl_down_sync` (`kernel_v1_impl.cuh:273`)
  — a standard warp shuffle, fine on RDNA.
- **wave32 is required and satisfied.** `kernel_v1.hip.cpp` hard-rejects
  `warpSize != 32` (CDNA/GCN wave64 unsupported). `gfx1103` (RDNA3), `gfx1150`
  and `gfx1151` (RDNA3.5) are all wave32 — the laptop parts are exactly the
  shape we want.

We're the "embedded HIP kernels directly" case that *worked* on gfx1103.

## The actual blocker is one line

`ucc::cl_hip()` (vendored `eda-infra-rs/ucc/src/compile.rs`):

```rust
// Default AMD targets: RDNA2 + RDNA3.
vec!["gfx1030".to_string(), "gfx1100".to_string()]
```

**We only emit code for `gfx1030` and `gfx1100` — both discrete.** No
`gfx1103`, no `gfx1150`, no `gfx1151`. That is why the CI runner works (it
reports `gfx1030`) and why a laptop wouldn't: not because HIP can't, but because
we never compiled for it.

There is already an escape hatch — `UCC_HIP_TARGETS`, comma-separated — in a
fork we control. Custom HIP kernels can be compiled for any arch the compiler
knows; only *rocBLAS* needs per-arch prebuilt libraries, and we don't use it.

## Revised conclusion

**Do not port to OpenCL.** The cost is a third 1400-line kernel plus a permanent
3× tax on every kernel change, and `sim` can't port at all (no device-wide
barrier). The premise that motivated it — "ROCm won't reach AMD laptops" — did
not survive contact: with two small fixes the existing HIP backend compiles for
every laptop arch and passes 13/14 goldens on real ROCm hardware. We were three
commits from ROCm, not one backend.

Both blockers were **ours**, not AMD's, and both were hidden by the same thing:
`hip-build` installs the CUDA Toolkit and targets `hip-runtime-nvidia`, so the
HIP path had only ever been compiled with CUDA headers and CUDA semantics on
hand. `HIP Tests (NVIDIA backend)` was an accurate job name that nobody read
literally.

What's left for laptops specifically is unproven but no longer speculative:
compiling for `gfx1103`/`gfx1150`/`gfx1151` works; whether they *run* correctly
needs silicon we don't have.

## Result of the compile test (2026-07-15)

Ran it on the AMD runner (ROCm 7.2.4) with
`UCC_HIP_TARGETS=gfx1030,gfx1100,gfx1103,gfx1150,gfx1151`. Two findings, and the
second is bigger than the spike's original question.

**1. hipcc accepts every laptop target.** The compiler was invoked as

```
clang++ --offload-arch=gfx1030 --offload-arch=gfx1100 --offload-arch=gfx1103 \
        --offload-arch=gfx1150 --offload-arch=gfx1151 ...
```

and raised no objection to any arch. The arch list is not a barrier — consistent
with the research: only *rocBLAS* needs per-arch prebuilt libraries, and we don't
use it.

**2. We have never compiled against real ROCm at all.** The build died here:

```
csrc/types.hpp:26:10: fatal error: 'math_constants.h' file not found
   26 | #include <math_constants.h>
1 error generated when compiling for gfx1030.
```

That's vendored `ulib/csrc/types.hpp`:

```cpp
#if defined(__NVCC__) || defined(__HIP_DEVICE_COMPILE__)
#include <math_constants.h>
```

The guard fires for HIP device compilation, but **`math_constants.h` is a CUDA
toolkit header**. ROCm ships `hip/hip_math_constants.h` instead. It has never
been caught because `hip-build` **installs the CUDA Toolkit** ("no GPU needed to
compile") and builds `hip-runtime-nvidia`: the HIP path has only ever been
compiled with CUDA headers on the include path.

So the `HIP Tests (NVIDIA backend)` job name is exact, and nobody read it that
literally: **the HIP backend is HIP-over-CUDA only.** It has never been built,
let alone run, on ROCm — on a laptop, a discrete Radeon, or anything else. Note
this fails on **`gfx1030`**, the arch we nominally support and the one our own
runner is. Laptop support was never the first blocker; it's the second.

This is a one-header fix in a fork we control, but it must be fixed before any
claim about ROCm — laptop or otherwise — can be tested.

## Result of actually fixing it (2026-07-15)

Two blockers, both invisible until something compiled against real ROCm. Both
fixed; the third finding is open.

**Blocker 1 — CUDA header on the HIP path.** `81184fa` ("Add HIP (AMD GPU)
backend support") widened ulib's guard from `#ifdef __NVCC__` to
`#if defined(__NVCC__) || defined(__HIP_DEVICE_COMPILE__)`, dragging the
CUDA-only `<math_constants.h>` into HIP device compilation. Upstream is
unaffected — its `#ifdef __NVCC__` is correct for a CUDA header; **the bug is
ours, introduced with the HIP patch**. Nothing uses the `CUDART_*` constants that
header provides; the macros beneath it want `nanf`/`nan`/`INFINITY` from
`<cmath>`. Fixed by including what's used.

**Blocker 2 — the lane mask is CUDA-shaped.** With the header fixed, the compile
reached our one arch-specific intrinsic and died:

```
amd_warp_sync_functions.h:297:62: error: static assertion failed due to
requirement 'sizeof(unsigned int) == 8': The mask must be a 64-bit integer.
```

CUDA's `__shfl_*_sync` takes a 32-bit mask; **HIP-on-AMD takes a 64-bit one**
(an AMD wave can be 64 lanes) and static-asserts it. Every call site in the
shared kernel was a hard compile error on ROCm, and invisible on CUDA and on
HIP-over-CUDA — which is all we had ever built. Fixed with a `lane_mask_t`
typedef (64-bit under `__HIP_PLATFORM_AMD__`, `unsigned` otherwise). It has to be
a *type*, not a constant: one mask is a ragged tail (`0xffffffff >> n`) whose
value must survive widening. No-op off AMD, and Metal goldens stayed 14/14.

### With both fixed: it builds, and it very nearly works

```
UCC_HIP_TARGETS=gfx1030,gfx1100,gfx1103,gfx1150,gfx1151
→ build: SUCCESS  (all five archs, incl. every laptop target)
```

And on the AMD runner's real gfx1030 — **the first time Jacquard has ever
executed on ROCm** — the goldens are byte-identical to the CpuBackend/Metal
captures:

| | |
| --- | --- |
| PASS | xprop, 2state, noreginit, reginit, dual_uart_events, apb_trace, apb_trace_xprop, multi_mem, vcd_axes, qspi_psram (+content), qspi_shared_bus (+content) |
| **FAIL** | **multi_mem_split** |

**13 of 14 on the first run — 14 of 14 once the one failure was fixed.** The
cross-backend equivalence the goldens assert (CpuBackend == Metal == CUDA ==
HIP) had never been tested against ROCm. It holds, and finding where it didn't
was worth the whole exercise.

### The one failure: a GPU memory fault on the staged path (#203, fixed)

```
Memory access fault by GPU node-1 on address 0x7501bce00000.
Reason: Page not present or supervisor privilege.
```

`multi_mem_split` is `multi_mem` with `--level-split 10`. The **non-split**
fixture passed on the same hardware and the same binary; only the staged path
faulted.

The cause was ours, in `simulate_block_v1`'s staged-IO read. Bit 31 of a word
index flags "read this cycle's inter-stage intermediates from the output slot"
(§5 of ADR 0015). The kernel decoded that flag by biasing the base pointer to
cancel it out of the subscript — with a **signed** `1 << 31`, which is
`INT_MIN`. So the pointer moved *+*2^31 words instead of −2^31, and the
subsequent `[idx]` (bit 31 set, so idx ≥ 2^31) added another 2^31: every staged
read landed 2^32 words — 16 GiB — past the buffer. Unreachable without
`--level-split`, since `staged_io_map` is empty when the design is one stage,
and unreachable in stage 0, which reads primary inputs and DFFs. Hence:
stage 0 survives, stage 1 faults.

**Why only ROCm.** The instinct that ROCm is a bounds-checking oracle was
right, but the mechanism is more interesting than "CUDA tolerates an OOB
access". nvcc narrows the address arithmetic to 32 bits, which truncates the
overflow back to the intended offset — so CUDA and HIP-over-CUDA were silently
*correct*, not silently wrong. Metal had the same decode written with an
unsigned `1u << 31` and was correct outright. ROCm was the only backend that
computed the address the way the language says to, and it faulted. The bug was
not that ROCm is strict; it was that we relied on a compiler to paper over
undefined behaviour, and one of them declined.

The fix decodes by clearing the flag from the *index* instead — the idiom
`CpuBackend` already used (`src/sim/cpu_reference.rs`) — in all three kernels.
That removes the UB rather than correcting its sign, so no backend's address
arithmetic can resurrect the class. `multi_mem_split` now passes on ROCm
byte-identically to the golden, and Metal is re-verified 14/14 locally.

Worth noting where it sat: level-split + SRAM is exactly where #186 lived (a
staged endpoint index read against the original AIG's accounting). Two staged-
index bugs in the same neighbourhood, the second only visible on a runtime that
checks. **Both were real bugs in Jacquard, surfaced by ROCm, not ROCm quirks.**

## Next steps

1. ~~Compile-only test.~~ **Done — see above.** It answered a bigger question
   than it asked. Fix the `types.hpp` CUDA-header leak in vendored `ulib`, then
   re-run; only then is "does it build for laptop archs" a meaningful question.
2. ~~Run the goldens on our own AMD runner.~~ **Done — 14/14.** The first run
   was 13/14; the one failure became **#203** and is fixed (a signed `1 << 31`
   put every staged-IO read 2^32 words out of bounds; see above). Traced with
   `AMD_SERIALIZE_KERNEL=3` to the exact kernel and dispatch — `rocgdb` hangs,
   that route is the one that works. The goldens are now guarded permanently by
   the `HIP Tests (ROCm backend)` job in `ci.yml`, which is worth keeping for
   its own sake: ROCm is the only backend that computes addresses exactly
   rather than narrowing them to 32 bits, so it is our only standing oracle for
   OOB/UB in the shared kernel.
3. **Then find real silicon.** Compiling is necessary, not sufficient. The
   cross-backend goldens make the check a byte-diff, not a judgement call. This
   is the step that needs a Strix/Phoenix laptop; no runner we have can stand in
   (ours is `gfx1030`, a different ROCm support tier).
   `scripts/amd-laptop-probe.sh` is a self-contained volunteer test: it compiles
   a ~40-line HIP program using exactly Jacquard's kernel surface (wave32 +
   `__shfl_down_sync` + `__syncthreads`, nothing else), runs it, cross-compiles
   for each laptop arch, and prints a pasteable report. No Jacquard build, no
   root, nothing installed.
3. **Only if 1 or 2 fails**, revisit portable compute — and then Vulkan, not
   OpenCL: it's what actually works on these parts today per the evidence above,
   it isn't deprecated, and the runner's `vulkan`/`cubecl` labels suggest prior
   thought. CubeCL being Rust-native likely beats hand-written OpenCL C as a
   third kernel.
4. **Fix the runner label** — it says `gfx1036`, hardware says `gfx1030`.

Still open: whether `sim` (not just cosim) matters on a laptop; if cosim-only is
acceptable the problem shrinks either way.

[20839]: https://github.com/ggml-org/llama.cpp/issues/20839
[13565]: https://github.com/ggml-org/llama.cpp/issues/13565
[5643]: https://github.com/ROCm/ROCm/issues/5643
[rocmlibs]: https://github.com/likelovewant/ROCmLibs-for-gfx1103-AMD780M-APU

[matrix]: https://rocm.docs.amd.com/en/latest/compatibility/compatibility-matrix.html
[radeon]: https://rocm.docs.amd.com/projects/radeon-ryzen/en/latest/index.html
[ryzlinux]: https://rocm.docs.amd.com/projects/radeon-ryzen/en/latest/docs/compatibility/compatibilityryz/native_linux/native_linux_compatibility.html
[ryzinstall]: https://rocm.docs.amd.com/projects/radeon-ryzen/en/latest/docs/install/installryz/native_linux/install-ryzen.html
[5339]: https://github.com/ROCm/ROCm/issues/5339
[ollama11451]: https://github.com/ollama/ollama/issues/11451
