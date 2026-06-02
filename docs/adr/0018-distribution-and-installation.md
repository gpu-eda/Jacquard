# ADR 0018 — Distribution and installation model

**Status:** Proposed.

**TL;DR.** In the context of shipping Jacquard to users (and to
docs-dogfooding agents), facing the fact that it is a GPU-compiled Rust
binary with vendored path-dependency submodules plus two companion
tools, we chose a **tiered, channel-per-artifact** model — Rust binaries
via GitHub Releases + `cargo-binstall` + a Homebrew tap (Metal), and the
Python `netlist-graph` companion via PyPI — accepting that we maintain
per-GPU-target release binaries and three coordinated release cadences,
rather than forcing everything through one channel.

## Context

"Install Jacquard" is not one artifact. The toolset is three pieces:

| Tool | Language | Role | GPU? |
|------|----------|------|------|
| `jacquard` | Rust | the simulator (`sim` / `cosim`) | yes — Metal / CUDA / HIP, compiled via the `ucc` build script |
| `opensta-to-ir` | Rust | SDF → timing-IR preprocessing (timing path) | no (CPU) |
| `netlist-graph` | Python | post-synthesis signal-name discovery; the companion the tracing docs (`signal-tracing.md`, `bus-tracing.md`) lean on | no |

Constraints that rule options out:

- **`jacquard` is not crates.io-publishable.** Its dependencies are
  vendored **path deps** (`vendor/eda-infra-rs/*` — a fork carrying
  in-flight patches), and the build needs git submodules, a GPU SDK, and
  a C++/CUDA/Metal compile. `cargo install` from source works but is slow
  and needs the full toolchain — not "easy."
- **`jacquard` is not a natural PyPI package.** It's a GPU binary; a
  maturin wheel would mean per-backend wheels (Metal macOS-arm64 only,
  CUDA huge and driver-coupled, HIP).
- **`netlist-graph` *is* PyPI-ready today** — self-contained
  (`networkx` + `click` only), a `netlist-graph` console script, no
  workspace path deps.

Today there are **no release artifacts**: install = clone + submodules +
`cargo build -r --features <backend>`. That blocks easy adoption and
makes docs-dogfooding (a fresh agent following the docs) start from a
heavy source build.

## Decision

Distribute each artifact through the channel that fits it:

1. **Rust binaries (`jacquard` + `opensta-to-ir`)** →
   - **GitHub Releases** prebuilt binaries, one per GPU target
     (`macos-arm64-metal`, `linux-x64-cuda`, `linux-x64-hip`).
   - **`cargo-binstall`** support via `[package.metadata.binstall]` so
     `cargo binstall jacquard` fetches the release asset.
   - **Homebrew tap** (`gpu-eda/homebrew-tap`) for the macOS/Metal path:
     `brew install gpu-eda/tap/jacquard`.
   - `opensta-to-ir` ships **alongside** `jacquard` (same release, same
     formula) — it's a sibling CPU bin in the same cargo workspace.
2. **`netlist-graph` (Python)** → **PyPI**: `uvx netlist-graph` /
   `pip install netlist-graph`. Versioned **independently** of the Rust
   bins.
3. **The simulator never ships via PyPI.** PyPI is for the Python
   companion only.

**Versioning:** the two Rust bins share the workspace version and a
single tag (coordinated); `netlist-graph` versions independently. This
is effectively Jacquard's first numbered release — see
[`release-process.md`](../release-process.md).

**Homebrew scope:** the formula installs the Rust bins only;
`netlist-graph` is documented as a separate `uvx`/pip line rather than a
formula dependency, keeping the formula simple and the Python tool
independently installable.

**Rollout is Metal-first:** the macOS-arm64 Metal binary ships now (the
self-hosted macOS runner exists). CUDA and HIP release binaries are
gated on the NVIDIA / AMD runners being stood up; until then those
targets remain source-build.

**Install tiers** (documented where each is first needed):

- pure functional cosim → `jacquard` only;
- signal-name debugging → add `netlist-graph`;
- timing / post-PnR → add `opensta-to-ir` + a PDK (`volare`/`ciel`).

## Alternatives considered

- **PyPI wheel for the simulator (maturin).** Rejected: per-backend
  wheels, CUDA wheel size and driver coupling, Metal macOS-arm64-only.
  PyPI fits the pure-Python companion, not the GPU binary. (This is the
  natural-sounding option; it doesn't survive contact with the GPU
  backends.)
- **crates.io publish + `cargo install`.** Blocked by the vendored
  path-dependency fork, and still requires a GPU SDK + a multi-minute
  compile. Not "easy install."
- **Container image as the primary channel.** Good for reproducible
  CUDA/Linux, but a poor fit for the macOS/Metal majority path and heavy
  for a quick `jacquard --help`. Kept as a possible *additional* channel
  (deferred), not the primary.

## Consequences

- One-line installs on the platforms that matter; docs-dogfood agents
  can `brew install` instead of building from source.
- Release CI must build a per-target matrix; the CUDA/HIP rows are
  runner-gated, so the first release is Metal-only and the matrix fills
  in as runners land.
- Three artifacts to release: two Rust bins (coordinated, one tag) and
  `netlist-graph` (independent). Coordinated Rust versioning keeps this
  to two cadences, not three.
- New cross-repo surface to own: a `homebrew-tap` repo and a PyPI
  project + publish credentials (trusted publisher).
- The stale `repository = "…/ChipFlow/Jacquard"` URLs (both
  `Cargo.toml`s) must be corrected to `gpu-eda` as part of this work.

## Walk-back options

- If maintaining per-backend release binaries proves too costly, fall
  back to **container images for CUDA/HIP** and keep Releases + Homebrew
  for Metal only — the channels are independent, so this is a per-target
  retreat, not a redesign.
- If the Homebrew tap is more upkeep than it's worth, drop it and keep
  `cargo-binstall` + the release tarballs; the formula is the thinnest
  layer to remove.

## Links

- Implementation phasing: [`../plans/distribution.md`](../plans/distribution.md).
- Release mechanics: [`../release-process.md`](../release-process.md).
