# ADR 0020 — Python engine as a bundled binary wheel

**Status:** Draft — decision deferred (2026-07-01). Not ratified; not scheduled.

> **Direction note (2026-07-01).** This ADR drafts a *subprocess-bundled binary
> wheel* (embed the CLI, keep PR #53's subprocess API). On review, a **native
> PyO3 binding** is the preferred long-term direction — an in-process engine is
> a materially better "first-class" Python surface than shelling out to a
> bundled binary, and if we're going to invest in per-platform wheels
> (cibuildwheel + dylib repair) either way, doing it once for a PyO3 extension
> avoids building the subprocess-wheel machinery only to replace it. We are
> **not doing this now**; the decision (subprocess-wheel-first vs. straight to
> PyO3) is deferred and tracked in [#161](https://github.com/gpu-eda/Jacquard/issues/161). The
> subprocess-wheel design below is preserved as the worked alternative and as
> the analysis of the shared hard part (per-platform wheels + vendoring the
> `libc++`/`libomp` runtime tail), which a PyO3 binding faces too.

**Relates to:** [ADR 0018](0018-distribution-and-installation.md) (distribution
model — this extends it with a Python channel), PR #53 (the subprocess API this
would adopt), [`docs/plans/python-engine-binary-wheel.md`](../plans/python-engine-binary-wheel.md)
(implementation phases + tracking).

## Context

Jacquard ships as a Rust CLI (`jacquard sim` / `cosim` / `map`). Batch
automation, regression sweeps, and result analysis are naturally Python work,
and PR #53 drafted a Python API for exactly that: `JacquardConfig`, `sim()` /
`map()`, `SimResult` / `DesignStats`, and a `run_regression()` harness.

PR #53 as drafted is a **pure-Python subprocess wrapper**: a `hatchling`
noarch wheel with `dependencies = []` that locates a *separately installed*
`jacquard` binary (`JACQUARD_BIN` env → `PATH` → `shutil.which`) and shells out
via `subprocess.run`. That means a user must install two things from two
channels — the binary (`brew` / `cargo binstall`, per ADR 0018) **and** the
Python package — and keep their versions in step by hand.

We want a **first-class** Python engine: `pip install jacquard` yields a
working simulator with no separate binary step. That is a distribution
decision PR #53 does not make, and it is the subject of this ADR.

Two forces shape it:

1. **The binary is self-contained *except* for a runtime-library tail.** The
   `sim` Metal kernel is embedded via `include_bytes!` (ADR 0018 / v0.2.1), so
   the relocated binary needs no sidecar `.metallib`. But it links Homebrew
   LLVM's `libc++` and `libomp` (via mt-kahypar/OpenMP), so a bare binary
   fails to launch without LLVM present — the same defect that broke the
   v0.2.1 install channels and is worked around today by the Homebrew formula's
   `depends_on "llvm"`. A wheel has no `depends_on`; those dylibs must travel
   **inside** the wheel.
2. **The GPU backend is a per-platform build.** `metal` (macOS/arm64) is the
   shipping backend; `cuda`/`hip` are Linux and heavy. A universal wheel is not
   possible; wheels are per-platform, and the backend matrix must be phased.

## Decision

Ship the Python engine as a **binary wheel that embeds the compiled `jacquard`
binary and its runtime libraries**, built with **cibuildwheel**, published to
PyPI. Keep PR #53's subprocess API and config/result model unchanged — the
wheel changes *packaging*, not the Python surface.

Concretely:

1. **Adopt PR #53's API** (`config`, `runner`, `result`, `regression`,
   `errors`) as the package's Python layer, moved into the uv workspace
   (`python/jacquard/`, a 5th workspace member alongside `netlist_graph`,
   `chipflow_harness`, `mcu_soc`, root).

2. **Embed the binary.** The wheel carries the release binary at
   `jacquard/_bin/jacquard` (per-platform). `runner.find_jacquard_binary()`
   grows a step **0**: prefer the packaged `_bin/jacquard`, then fall back to
   its existing env → `PATH` → `which` chain (so a dev pointing `JACQUARD_BIN`
   at a local build still wins, and a source checkout with no embedded binary
   still works).

3. **Vendor the runtime tail into the wheel.** cibuildwheel's repair step
   (`delocate` on macOS, `auditwheel` on Linux) copies `libc++`/`libomp` (and
   any other non-system dylib the binary links) into the wheel and rewrites the
   install names, so `pip install` needs no Homebrew LLVM. This is the crux and
   the main risk; the plan spikes it first.

4. **Phase the platform matrix** (`docs/plans/...`):
   - **P1 — macOS/arm64 + Metal.** The shipping backend; validates the whole
     embed-plus-delocate story on the platform we already release for.
   - **P2 — Linux/x86_64 CPU fallback.** The cosim CPU backend (ADR 0017) with
     no GPU runtime, so `pip install` works in plain CI containers.
   - **P3 — CUDA / HIP.** Gated on ADR 0018 Phase 4 (prebuilt GPU binaries) and
     on the wheel-size / CUDA-runtime questions below; may ship as separate
     extras (`jacquard[cuda]`) or a manylinux variant rather than the default
     wheel.

5. **Publish via the established OIDC path.** Mirror
   `publish-netlist-graph.yml` (trusted publishing, no stored token; TestPyPI
   dry-run on `workflow_dispatch`, real PyPI on tag). cibuildwheel replaces the
   single `uv build` with a per-platform build matrix.

6. **Version with the binary, not independently.** The wheel embeds a specific
   `jacquard` build, so its version tracks the binary release (extend
   `scripts/bump_version.py`), unlike `netlist-graph` which versions
   independently. A wheel's embedded binary and its Python API are one artifact.

### Why subprocess, not native (PyO3) bindings

A native PyO3/maturin binding would give in-process state access (drive the
sim, peek signals without a subprocess) — a deeper "engine." We defer it:

- It multiplies the build matrix by the GPU-feature axis *inside* the extension
  module, where the runtime-library and feature-gating problems are far harder
  than embedding an already-built binary.
- PR #53's subprocess API is backend-agnostic: it works for `metal`/`cuda`/`hip`
  unchanged, because the binary owns the backend. A binding does not.
- The subprocess wheel delivers the user-visible win (`pip install` → working
  simulator) now; PyO3 is a strictly larger follow-on that a later ADR can take
  up if in-process access becomes a real requirement.

## Consequences

- **`pip install jacquard` is self-contained** on supported platforms — the
  headline win. The two-channel version-skew problem goes away.
- **cibuildwheel + delocate/auditwheel become release-critical infrastructure.**
  A new failure surface (dylib repair) that the plan de-risks with a P1 spike
  and a post-build "install into a clean venv and run `sim`" smoke gate,
  mirroring the existing relocated-tarball user-acceptance gate.
- **Wheels are large** (embedded binary + vendored dylibs; CUDA especially).
  Acceptable for Metal/CPU; a gating question for P3.
- **The PyPI name `jacquard` must be secured** (see open questions). Until
  then, TestPyPI validates the pipeline.
- **`netlist-graph` stays a separate, independently-versioned noarch package.**
  This ADR is only about the simulator engine wheel.

## Alternatives considered

- **Ship PR #53 as-is (pure-Python noarch + separate binary).** Simplest, no
  cibuildwheel, and a fine *interim* — but it is not the first-class,
  single-command install the goal calls for; it defers the real distribution
  decision rather than making it. Rejected as the end state; effectively
  subsumed as pre-P1 (the API lands first, the embed follows).
- **Native PyO3/maturin bindings.** Highest ceiling, deferred — see above.
- **Document-and-require the LLVM runtime** (as the raw tarball does) instead of
  vendoring dylibs. Pushes the v0.2.1-class failure onto every `pip` user;
  rejected — self-containment is the entire point.

## Open questions (tracked in the plan)

- **PyPI name.** Is `jacquard` available/securable? If not, `jacquard-eda` or
  `gpu-eda-jacquard`, aliasing the import name. Blocks nothing until publish.
- **CUDA/HIP wheel viability.** Size and CUDA-runtime bundling may make P3 an
  extras/manylinux special case rather than a default wheel — decide in P3 with
  data, and `log`/document any platform the wheel silently omits.
- **delocate coverage of `libomp`.** OpenMP runtime vendoring has known
  sharp edges (duplicate-runtime aborts if a user's other packages also load
  libomp); the P1 spike must confirm a clean load in a mixed environment.
