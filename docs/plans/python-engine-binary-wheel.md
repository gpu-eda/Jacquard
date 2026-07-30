# Plan — Python engine as a bundled binary wheel

Implementation plan for [Decision 0020](../architecture/decisions/0020-python-engine-binary-wheel.md):
turn PR #53's subprocess API into a `pip install jacquard` self-contained
binary wheel built with cibuildwheel.

> **Deferred (2026-07-01).** Decision 0020 is a **draft** — a native PyO3 binding is
> the preferred long-term direction, and whether we build this subprocess-wheel
> path at all is deferred ([#161](https://github.com/gpu-eda/Jacquard/issues/161)). **P0** (adopt
> PR #53's API into the uv workspace) is worth doing regardless — it's the
> Python surface either approach exposes. **P1–P3** (embed-and-delocate the
> subprocess binary) are on hold pending the subprocess-vs-PyO3 decision; note
> the shared hard part (per-platform wheels + `libc++`/`libomp` vendoring)
> carries over to a PyO3 extension either way.

## Guiding constraints

- **The Python surface is PR #53's** — `config` / `runner` / `result` /
  `regression` / `errors`. This plan changes packaging and adds a binary; it
  does not redesign the API.
- **Reuse, don't reinvent, the release machinery.** The binary to embed is the
  one `release.yml` already builds (`cargo build --release --features metal
  --bin jacquard`, metallib embedded). The publish path is
  `publish-netlist-graph.yml`'s OIDC trusted-publishing shape. The smoke idea
  is `scripts/ci/user_acceptance_smoke.sh` (install relocated → run `sim`).
- **Every phase ends green in CI** with an install-into-clean-venv smoke test,
  and any platform the wheel does *not* cover is `log`-ged, not silently
  dropped.

## P0 — Land the API in the workspace (no binary yet)

Adopt PR #53 as a uv workspace member; ship nothing to PyPI yet.

1. Move #53's `python/jacquard/` in as the 5th workspace member; wire it into
   the root `pyproject.toml` workspace list next to `netlist_graph` /
   `chipflow_harness` / `mcu_soc`.
2. Keep `find_jacquard_binary()`'s current env → `PATH` → `which` chain (no
   embedded binary yet); it resolves a dev's local `target/release/jacquard`.
3. CI: a `python-api` job — `uv sync`, `ruff`, `pytest` — plus one integration
   test that builds `jacquard` and drives `sim()` against a tiny fixture
   (guarded like the other Metal jobs; reuses the `metal-build` artifact from
   the build/test split).
4. **Exit:** #53's tests green in CI; `from jacquard import sim` works against a
   locally built binary. #53 can be closed/superseded by this branch.

## P1 — macOS/arm64 + Metal binary wheel (the spike + the crux)

The whole ADR rests on "embed the binary + vendor its dylibs into a wheel that
launches with no Homebrew LLVM." Prove it here on the platform we already
release for.

1. **Embed step.** Package build copies the release binary to
   `jacquard/_bin/jacquard`. `find_jacquard_binary()` gains step 0: prefer the
   packaged `_bin/jacquard`; existing chain remains the fallback.
2. **cibuildwheel + delocate.** Configure `cibuildwheel` for `macos/arm64`,
   with `delocate` repairing the wheel — copying `libc++`/`libomp` in and
   rewriting install names. Confirm with `otool -L` that the repaired binary
   references `@loader_path`-relative dylibs, not `/opt/homebrew`.
3. **Clean-environment smoke (the gate).** In a runner *without* Homebrew LLVM
   on the load path (or with it hidden), `pip install` the wheel into a fresh
   venv and run a real `sim` — the `pip`-user analogue of the relocated-tarball
   user-acceptance gate. This is the test that would have caught v0.2.1.
4. **libomp duplicate-runtime check.** Load jacquard in a venv that also imports
   a numpy/scipy stack (which ship their own libomp) and confirm no
   duplicate-OpenMP abort.
5. **Exit:** a macOS/arm64 wheel that `pip install`s and runs `sim` on a clean
   box; TestPyPI upload validated via `workflow_dispatch`.

## P2 — Linux/x86_64 CPU-fallback wheel

Make `pip install jacquard` work in a plain Linux CI container (no GPU).

1. Build the cosim **CPU backend** (Decision 0017) for `manylinux`; `auditwheel`
   repairs the (smaller, no-GPU) dylib set.
2. Same clean-venv `sim`/`cosim` smoke, in a stock `manylinux`/ubuntu container.
3. Document the backend the Linux wheel provides (CPU) vs. what needs P3.
4. **Exit:** Linux CPU wheel installs and runs in a bare container.

## P3 — CUDA / HIP (gated, likely extras)

1. **Gate:** Decision 0018 Phase 4 (prebuilt CUDA/HIP binaries) must exist first —
   this reuses those binaries, it does not invent GPU CI.
2. Decide with data: default manylinux wheel vs. `jacquard[cuda]` extra vs.
   a separate package — driven by wheel size and CUDA-runtime bundling. `log`
   and document whichever platforms the default wheel omits.
3. **Exit:** a documented, working install path for at least one GPU backend on
   Linux, or a recorded decision to keep GPU on the tarball/binstall channel.

## Publish pipeline (built across P1–P3)

- New `publish-jacquard-wheel.yml`, modeled on `publish-netlist-graph.yml`:
  cibuildwheel build matrix → artifact → **TestPyPI** on `workflow_dispatch`
  (dry-run) → **PyPI** via OIDC trusted publishing on a release tag. No stored
  token; configure the trusted publisher once (mind the case-sensitive
  `gpu-eda/Jacquard` repo claim — the same gotcha `netlist-graph` hit).
- Version tracks the binary release via `scripts/bump_version.py` (the embedded
  binary and the wheel are one artifact), not `netlist-graph`'s independent line.
- Extend `docs/installation.md` (a `pip install jacquard` path) and
  `docs/release-process.md` (the wheel channel + its staging-validation gate).

## Open questions (from the ADR)

- **PyPI name** `jacquard` — confirm availability; fall back to `jacquard-eda`
  with an aliased import name if taken. Blocks only the first real publish.
- **CUDA wheel size / runtime** — P3 decision, with data.
- **libomp duplicate runtime** — must be cleared in the P1 spike (step 4) before
  committing to delocate as the vendoring mechanism.
