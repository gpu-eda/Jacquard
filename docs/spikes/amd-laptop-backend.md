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

## Open questions

1. **Does `gfx1103` (Phoenix/Hawk Point) work under ROCm today?** Sources
   conflict. It's the volume part in current AMD laptops, so the answer changes
   the picture.
2. **Does OpenCL actually reach these parts** (Mesa rusticl / AMD's runtime) on
   distros users have? OpenCL's reputation for breadth and its current AMD
   reality may have diverged the way ROCm's did — worth checking rather than
   assuming.
3. **Vulkan compute as the alternative.** Same no-device-barrier constraint, but
   not deprecated anywhere, and the AMD runner is labelled `vulkan` + `cubecl`,
   suggesting someone already thought about this. CubeCL is Rust-native, which
   may beat hand-written OpenCL C as a third kernel.
4. **How much does `sim`-on-laptop matter?** If cosim-only is acceptable, the
   whole problem shrinks.

## Next step

Research what the local-LLM community has learned running on `gfx1103` /
`gfx1150` / `gfx1151` — llama.cpp, ollama, LM Studio have been driving these
parts in anger for far longer than any vendor matrix reflects, across ROCm,
Vulkan, and OpenCL backends. Their bug trackers are the best available evidence
of what actually works on real laptops, as opposed to what's on a support list.

[matrix]: https://rocm.docs.amd.com/en/latest/compatibility/compatibility-matrix.html
[radeon]: https://rocm.docs.amd.com/projects/radeon-ryzen/en/latest/index.html
[ryzlinux]: https://rocm.docs.amd.com/projects/radeon-ryzen/en/latest/docs/compatibility/compatibilityryz/native_linux/native_linux_compatibility.html
[ryzinstall]: https://rocm.docs.amd.com/projects/radeon-ryzen/en/latest/docs/install/installryz/native_linux/install-ryzen.html
[5339]: https://github.com/ROCm/ROCm/issues/5339
[ollama11451]: https://github.com/ollama/ollama/issues/11451
