# Plan: CI build/test split via reusable workflows

> **Status (2026-07-06):** Core split **implemented and green** on PR #172
> (run 28785097079) — `build.yml` + `test.yml` + composite actions + thin
> `ci.yml`. Faithful move (each backend keeps its current tests). Remaining:
> branch-protection check-name swap (below), then the four follow-ups at the
> end of this doc.

## Goal

Replace the organically-grown 2056-line `ci.yml` with a clean **build → test**
pipeline: one reusable **build workflow** that compiles each target
configuration on a cheap standard runner and uploads the binary as an artifact,
and one reusable **test workflow** that runs the *same* batch of tests against
those prebuilt binaries on the appropriate runners (GPU runners for the GPU
backends, a standard runner for the CPU-only config). A thin `ci.yml`
orchestrator wires them together for PRs; `release.yml` reuses the *same* build
workflow so published binaries are never a separate, drifting build.

## Why this shape

Three facts about the current CI drove the design (all verified against the
tree, 2026-07-06):

1. **The build/test seam already exists but is inconsistent.** `metal-build →
   metal`, `cuda-build → cuda`/`cuda-blackwell`, `hip-build → hip-on-nvidia`
   already split compile (standard runner) from GPU execution (paid/self-hosted
   runner) via `upload/download-artifact`. But the *test* half is not uniform:
   `cuda`/`blackwell` share the backend-agnostic
   `scripts/ci/gpu_test_suite.sh`, while `metal` and `hip-on-nvidia` run ad-hoc
   one-off steps. Unifying every GPU job onto the shared suite is the core win.

2. **The backends are additive, not exclusive — a fat binary is coming.**
   `ulib::Device` (`vendor/eda-infra-rs/ulib/src/lib.rs:79`) is a runtime enum
   with independently `#[cfg]`-gated `CUDA(u8)`/`HIP(u8)`/`Metal(u8)` variants;
   `UVec` carries per-backend buffer arrays in one struct; kernel entry points
   are pre-suffixed `_cuda`/`_hip`. So `--features cuda,hip` is a supported
   config today, and a single CUDA+HIP binary is a jacquard-only follow-up (no
   `ulib` fork). The matrix must therefore be designed so `cuda` + `hip` can
   later **collapse into one artifact** without reshaping the workflows.

3. **Linux users currently build from source.** `release.yml` publishes only a
   macOS/Metal binary; there is no Linux release artifact (README:48 says so).
   The build workflow already produces exactly the Linux CUDA/HIP binaries that
   are missing — today they're thrown away (`retention-days: 1`). Making build a
   reusable workflow lets `release.yml` publish them later for free.

## Build matrix — 4 artifacts across 2 platforms

`synth` is **uniform** on every artifact (decision: published binaries and test
binaries all carry the RTL on-ramp).

| Artifact | Build runner (standard) | Build command |
|---|---|---|
| `jacquard-cpu` | `ubuntu-latest` | `cargo build -r --features synth` |
| `jacquard-cuda` | `ubuntu-22.04` | `JACQUARD_CUDA_ARCH=all-major cargo build -r --features cuda,synth` |
| `jacquard-hip` | `ubuntu-22.04` | `cargo build -r --features hip,synth` (HIP_PLATFORM=nvidia today) |
| `jacquard-metal` | `macos-latest` (free) | `cargo build -r --features metal,synth` |

Realized as `build.yml` (`on: workflow_call`) with a matrix over the four
configs (`runs-on: ${{ matrix.runner }}`). The divergent toolchain setup
(CUDA-toolkit install, ROCm install, Homebrew LLVM) is extracted into
**composite actions** under `.github/actions/setup-{cuda,hip,metal}` so the same
setup is shared between the build job and the test job that needs the matching
runtime — the biggest current copy-paste. Submodule-init is likewise a small
composite action (the required-submodule lists differ by job and are duplicated
today).

## Runner strategy — GitHub-hosted for gating, self-hosted for perf

**Principle (decision):** functional *gating* CI runs entirely on GitHub-hosted
runners so jobs parallelize; **self-hosted boxes are reserved for performance
measurement**, not functional gates. The macOS migration to the GitHub-hosted
`macos-latest-xlarge` is complete (it replaced a self-hosted mac runner that
serialized jobs). `tesla4-runner` is a GitHub-hosted T4, so CUDA/HIP gating
parallelizes too. The self-hosted `nvidia1` box (today's `cuda-blackwell`, and a
future AMD runner) drops out of the near-term functional gate into the
"perf/async coverage" bucket.

This may need to be revisited for AMD functional tests in the future.

## Test matrix — same suite, appropriate runners

| Config | Test runner (GitHub-hosted) | Test body |
|---|---|---|
| CPU | `ubuntu-latest` (no GPU) | `gpu_test_suite.sh`-equivalent CPU paths + cosim-cpu regression |
| CUDA | `tesla4-runner` (T4) | `gpu_test_suite.sh cuda` |
| HIP | `tesla4-runner` (T4, HIP-on-NVIDIA) | `gpu_test_suite.sh hip` |
| Metal | `macos-latest-xlarge` | `gpu_test_suite.sh metal` |

Non-gating / deferred to the perf-and-coverage bucket on self-hosted `nvidia1`:
`cuda-blackwell` (sm_120, already declared not-required) and a future
`[self-hosted, hip, amd]` real-AMD runner. These are async extra coverage, kept
out of the merge gate so an offline box never blocks a merge.

`test.yml` (`on: workflow_call`) downloads the artifacts and runs the shared
suite per config. **Metal and HIP are migrated onto `gpu_test_suite.sh`** so all
four backends run the identical batch — that also feeds `backend-equivalence`
cleaner inputs. The pure-Rust compile-in-place jobs (`cargo test --lib`, the
cell-model-IR crate tests, `opensta-to-ir`, the synth input-classifier tests)
**cannot** consume a prebuilt binary — they build their own test harnesses — so
they stay as self-building jobs inside `test.yml`, not artifact consumers.

## Job migration map

Downstream jobs fold into `test.yml` (scope decision: full reorg):

| Current job | New home | Notes |
|---|---|---|
| `test`, `synth-onramp`, `opensta-to-ir-tests`, `lint`, `benchmark`, `docs` | `test.yml` (self-building) | compile-in-place; no artifact |
| `cosim-cpu` | `test.yml`, consumes `jacquard-cpu` | now `--features synth` |
| `metal`, `cuda`, `hip-on-nvidia` | `test.yml` (GitHub-hosted), consume GPU artifacts | **all** onto `gpu_test_suite.sh` |
| `cuda-blackwell` | non-gating perf/coverage on self-hosted `nvidia1` | keep artifact reuse; out of merge gate |
| `prepare-mcu-soc-jtir`, `mcu-soc-metal` | `test.yml` | consumes `jacquard-metal` |
| `jtag-minimal-cosim{,-server,-openocd}` | `test.yml` | consume `jacquard-metal` |
| `cvc-reference`, `timing-comparison` | `test.yml` | **real gate** on inv_chain timing (keep) |
| `mcu-soc-cvc`, `mcu-soc-comparison` | `test.yml` | **advisory guide, not gate** — see below |
| `backend-equivalence` | `test.yml` | keep; grows with fat binary + AMD |

### CVC handling (decision)

- `cvc-reference` + `timing-comparison` stay **gating** — they validate
  `inv_chain_pnr` timing (800–2000ps range + VCD diff) against the event-driven
  CVC reference.
- `mcu-soc-cvc` + `mcu-soc-comparison` stay **`continue-on-error: true`
  (advisory)** but must **emit a `$GITHUB_STEP_SUMMARY` of the current CVC↔Metal
  differences** — acting as a *guide* (surfaced diffs a human reads), not a
  merge gate. Today they run non-blocking and produce no digestible summary;
  this closes that gap without committing the fragile CVC-from-source build
  (`-O0` segfault workaround) as a required check.

## Orchestration

```
ci.yml (on: pull_request, push)
  └─ changes (paths filter)
  └─ build.yml   (workflow_call, if code changed)   → uploads 4 artifacts
  └─ test.yml    (workflow_call, needs: build)       → downloads + runs suites
release.yml
  └─ build.yml   (workflow_call)                     → reuse for publish (later)
```

Reusable workflows run as jobs in the *same* run, so cross-workflow
`upload/download-artifact` works and PR gating stays intact (unlike a
`workflow_run` trigger, which would fire after completion and not gate the PR).
The "build did not succeed → fail the required test check (not skip)" gate
pattern from the current jobs is preserved.

## Follow-ups (explicitly out of scope for this PR)

1. **Fat CUDA+HIP binary** — jacquard-only runtime dispatch (convert the
   `#[cfg(feature="cuda")]`/`hip` sim/cosim gates in `src/bin/jacquard.rs` to a
   device probe). Collapses `jacquard-cuda` + `jacquard-hip` matrix rows into
   one. See `[[project_fat_cuda_hip_binary_feasible]]`.
2. **AMD/ROCm runner on nvidia1** — register `[self-hosted, hip, amd]` mirroring
   the existing blackwell runner; makes HIP testing *real* AMD instead of
   HIP-on-NVIDIA, and retires the `HIP_PLATFORM=nvidia` trick.
3. **Linux release binaries** — have `release.yml` call `build.yml` to publish
   `jacquard-cuda` (and later the fat binary). Needs the runtime-lib story
   (both `cudart`+`amdhip64` present, or lazy `dlopen`) resolved for a clean
   single-file distribution.
4. **Move `release-metal` off self-hosted** — `release.yml`'s `release-metal`
   still runs on self-hosted `macos-runner-1`; migrate it to GitHub-hosted
   `macos-latest` (consistent with freeing the mac box for perf) or, better,
   have it consume the `jacquard-metal` artifact from `build.yml`.
5. **Perf harness on `nvidia1`** — repurpose the freed self-hosted boxes for
   performance measurement (Blackwell sm_120 + real AMD), decoupled from the
   functional gate.

## Sequencing / risk

Ship this CI restructure **first and independently** — it keeps `cuda`/`hip` as
separate artifacts (small blast radius) and does not depend on the fat binary or
AMD runner landing. The matrix is shaped so (1) and (2) become config changes,
not rewrites. Main risk is the usual required-check plumbing (a skipped required
check counts as passing) — mitigated by carrying over the explicit build-gate
steps and validating on a throwaway branch before flipping branch protection.
