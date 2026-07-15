# Spike — reaching AMD laptops

**Status:** In progress. Findings below are what's established so far; the
compute-API question is open.

**Question:** what does Jacquard need in order to run on an AMD laptop, and is
the answer HIP/ROCm, OpenCL, Vulkan, or something else?

## Why this is a question at all

The `HIP Tests (NVIDIA backend)` job builds `hip-runtime-nvidia` and runs on
`tesla4-runner`: HIP-over-CUDA, never ROCm. The org's self-hosted AMD runner sat
online and idle because nothing targeted it (#198).

Probing it (2026-07-15) showed the box itself is fine — ROCm 7.2.4, `hipcc` (HIP
7.2.53211), `hipconfig --platform` = `amd`, and a trivial HIP kernel compiles and
runs correctly on the GPU with **no** `HSA_OVERRIDE_GFX_VERSION`:

```
--- native ---
result: 1 2 3 4 5 6 7 8
NATIVE: OK
```

But it reports **`gfx1030`**, despite a `gfx1036` runner label. `gfx1030` is
RDNA2 *discrete*-class and sits on ROCm's main compute compatibility matrix. It
is a different support tier from any laptop part. **Green HIP CI on that runner
would tell us nothing about an AMD laptop.** That is the finding that started
this spike.

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

**Do not port to OpenCL on this evidence.** The cost is a third 1400-line kernel
plus a permanent 3× tax on every kernel change, and `sim` can't port at all
(no device-wide barrier). The premise that motivated it — "ROCm won't reach AMD
laptops" — is not what the evidence says for a kernel shaped like ours.

**Try the one-line thing first:**

```
UCC_HIP_TARGETS=gfx1030,gfx1100,gfx1103,gfx1150,gfx1151 cargo build -r --features hip
```

If that compiles and the goldens pass on real laptop silicon, AMD-laptop support
costs a default-list change in a vendored fork, not a backend.

## Next steps

1. **Compile-only test, cheap and immediate.** `hip-build` already compiles
   without a GPU on `ubuntu-22.04`, and the AMD runner has ROCm 7.2.4 + hipcc.
   Add the laptop targets to `UCC_HIP_TARGETS` and see whether the kernel builds
   for them. Answers "can we even emit code for these" for free.
2. **Then find real silicon.** Compiling is necessary, not sufficient — the
   goldens must pass. The cross-backend goldens make that a byte-diff, not a
   judgement call. This is the step that needs a Strix/Phoenix laptop; no runner
   we have can stand in for it (ours is `gfx1030`).
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
