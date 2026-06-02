# Plan: distribution & easy install

Implements [ADR 0018](../adr/0018-distribution-and-installation.md). Goal:
a fresh user (or a docs-dogfooding agent) can install Jacquard in one
line on macOS/Metal, with CUDA/HIP following as runners land.

## Artifacts & channels (from ADR 0018)

| Artifact | Channel | Blocked on |
|----------|---------|------------|
| `jacquard` (Metal) | GitHub Release + `cargo-binstall` + Homebrew tap | — (Phase 1a done — relocatable) |
| `opensta-to-ir` | same release/formula as `jacquard` | — |
| `jacquard` (CUDA / HIP) | added to the release matrix | NVIDIA / AMD runners |
| `netlist-graph` | PyPI | — |

## Phase 0 — Metadata hygiene (no new infra)

- Fix `repository` URL `ChipFlow/Jacquard` → `gpu-eda/Jacquard` in
  `Cargo.toml` and `crates/opensta-to-ir/Cargo.toml`.
- Decide the release version (this is effectively `0.1.0`, the first
  numbered release) and confirm `CHANGELOG.md` exists / is rolled per
  `release-process.md`.
- Add `[package.metadata.binstall]` to `Cargo.toml` describing the
  release asset name template (e.g.
  `{ name }-{ version }-{ target }{ archive-suffix }`).

## Phase 1a — Relocatable Metal binary (code prerequisite) ✅ Done

Done in d2afde4: the metallib is embedded via `include_bytes!` +
`new_library_with_data`, verified by running the release binary copied
outside `target/`. The original problem, for the record:

**Blocker found while scoping Phase 1.** `MetalSimulator::new`
(`cosim_metal.rs:337`) loads the GPU kernel via
`new_library_with_file(env!("METALLIB_PATH"))` — a **compile-time
absolute path** into the build's `target/.../out/ucc_metal/`. A prebuilt
binary moved to another machine panics with `Failed to load metallib`:
the `.metallib` isn't bundled and the path doesn't exist there.

A distributable binary must locate its kernel without that build-tree
path. Preferred fix: **embed the metallib in the binary** —
`include_bytes!(env!("METALLIB_PATH"))` + `new_library_with_data`, so the
binary is self-contained (no sidecar file). Alternative: ship the
`.metallib` next to the executable and resolve relative to
`current_exe()`, with the `env!` path as a dev fallback. Embedding is
cleaner for distribution.

(CUDA/HIP likely have an analogous kernel-loading assumption; check when
Phase 4 lands.)

This must merge before Phase 1 produces a usable artifact.

## Phase 1 — Metal release CI (after 1a)

A `release.yml` workflow triggered on `v*` tags:

- On the self-hosted macOS runner: `cargo build -r --features metal
  --bin jacquard` and `cargo build -r --bin opensta-to-ir`.
- Package both binaries into `jacquard-<version>-macos-arm64-metal.tar.gz`
  (+ a `.sha256`).
- `gh release create` / upload the asset. Body = the CHANGELOG section.
- Smoke-test the packaged binary (`jacquard --version`, a tiny
  `tests/apb_trace` cosim) before publishing.

Deliverable: tagging `v0.1.0` produces a downloadable Metal binary.

## Phase 2 — Homebrew tap

**Tap repo provisioned:** [gpu-eda/homebrew-tap](https://github.com/gpu-eda/homebrew-tap)
created via `brew tap-new` (brew `test-bot` CI + `pr-pull` bottle
publishing). **Formula staged** at `packaging/homebrew/jacquard.rb`
(brew-style clean apart from homebrew-core-only Sorbet sigils; installs
`jacquard` + `opensta-to-ir`, tests `--version`). Remaining:

- At the first release: open a PR to the tap adding `Formula/jacquard.rb`
  with the released `url` / `version` / `sha256` (the release emits a
  `.sha256`). The tap CI installs + tests it on the PR.
- Subsequent releases: `brew bump-formula-pr`, or a release-CI step that
  opens the PR automatically (stretch).
- Then `brew install gpu-eda/tap/jacquard` → both bins on `PATH`.

See [`../../packaging/README.md`](../../packaging/README.md).

## Phase 3 — netlist-graph → PyPI

**Workflow written:** `.github/workflows/publish-netlist-graph.yml`
builds the wheel (`uv build --package netlist-graph`) and publishes via
PyPI **trusted publishing** (OIDC) on `netlist-graph-v*` tags;
`workflow_dispatch` is a build-only dry-run. The wheel + sdist build
cleanly (verified locally); `license = "Apache-2.0"` + URLs added to the
package metadata. Remaining (needs maintainer action):

- Create/own the `netlist-graph` PyPI project and configure its trusted
  publisher (owner `gpu-eda`, repo `Jacquard`, workflow
  `publish-netlist-graph.yml`, environment `pypi`).
- Result: `uvx netlist-graph …` / `pip install netlist-graph` work.

## Phase 4 — CUDA / HIP release binaries (runner-gated)

- Add `linux-x64-cuda` and `linux-x64-hip` rows to the `release.yml`
  matrix once the NVIDIA / AMD self-hosted runners are registered
  (tracks the same work re-enabling CUDA/HIP CI).
- Same package/upload/smoke-test shape as Metal.

## Phase 5 — Install docs (depends on 1–3)

- New `docs/installation.md` (in `SUMMARY.md`) presenting the tiers:
  `brew install` / `cargo binstall` for the simulator, `uvx
  netlist-graph` for signal analysis, `opensta-to-ir` + PDK for timing.
  Source-build remains documented as the fallback / contributor path.
- Trim the README "Build" section to point at the install page for users
  while keeping the from-source instructions for contributors.

## Phase 6 — Container image (optional, deferred)

- `ghcr.io/gpu-eda/jacquard:cuda` for reproducible Linux/CUDA runs.
  Deferred per ADR 0018; revisit if CUDA release binaries prove awkward.

## Verification

- A clean checkout-free install on a second macOS machine (or a fresh
  shell with no repo): `brew install …` then run a `tests/apb_trace`
  cosim end to end.
- `cargo binstall jacquard` fetches and runs without a Rust toolchain
  build.
- `uvx netlist-graph search <netlist> psel` works with no repo clone.
- **Then** the docs-dogfood: a Sonnet agent installs via the docs and
  attempts real tasks, reporting friction (the original motivation).

## Decisions (confirmed)

- **Versioning:** coordinated version across the two Rust bins
  (`jacquard` + `opensta-to-ir`), single tag; `netlist-graph` versioned
  independently.
- **Homebrew scope:** the formula installs the Rust bins only; the docs
  carry the `uvx netlist-graph` line separately. `netlist-graph` is not a
  formula dependency.
- **Tag scheme:** `v<X.Y.Z>` triggers the Rust release;
  `netlist-graph-v<X.Y.Z>` triggers the PyPI publish. The two workflows
  filter on their own tag prefixes so they never collide.
