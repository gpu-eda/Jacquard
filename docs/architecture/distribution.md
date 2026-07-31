# Distribution

*Reference — the current design as it stands in the code. The why lives in the
decision records linked from each section; the not-yet-built work is under
[Implementation status](#implementation-status). Present tense means "true
today"; if a claim here is stale, the code moved and this doc didn't. This
page is the architecture view of *why* Jacquard ships the way it does; for the
install commands themselves see [Installation](../installation.md), and for
the mechanics of cutting a release see [Release process](../release-process.md).*

Jacquard is three artifacts, not one: the `jacquard` simulator (Rust, GPU-compiled),
its sibling CPU tool `opensta-to-ir`, and the Python companion `netlist-graph`.
Each ships through the channel that fits it, rather than forcing all three
through one mechanism. [Decision 0018](decisions/0018-distribution-and-installation.md)
is the rationale; this page describes what's live.

## Channels

**`jacquard` + `opensta-to-ir`** ship together, one release, one tag, coordinated
versioning via `scripts/bump_version.py`:

- **GitHub Releases** — a prebuilt tarball per GPU target
  (`jacquard-<version>-macos-arm64-metal.tar.gz` today). The Metal kernel is
  embedded in the binary (`include_bytes!` + `new_library_with_data`), so the
  tarball needs no sidecar `.metallib`.
- **`cargo binstall --git`** — reads the `[package.metadata.binstall]` pkg-url
  override in `Cargo.toml` and fetches the release tarball instead of
  compiling. The package name is `jacquard-sim`, though the binary it installs
  is still `jacquard`. `Cargo.toml` explains why: the crate name `jacquard` is
  already taken on crates.io by an unrelated project.
- **Homebrew tap** (`gpu-eda/homebrew-tap`) — `brew install gpu-eda/tap/jacquard`
  installs both bins and declares `depends_on "llvm"`, covering the runtime
  dependency described below. A parallel `homebrew-tap-prerelease` tap serves
  release-candidate tags for staging validation.

`jacquard` is not published to crates.io: its dependencies are vendored path
deps (`vendor/eda-infra-rs/*`, a fork carrying in-flight patches), so
`cargo install jacquard-sim` from the registry isn't possible. `cargo binstall
--git` sidesteps this by reading the binstall metadata straight from the repo
rather than from a registry entry.

**`netlist-graph`** ships independently, via PyPI, with its own version and
its own OIDC-trusted-publish workflow (`publish-netlist-graph.yml`). It has no
GPU dependency and no path deps, so it's a plain noarch wheel — `uvx
netlist-graph` or `pip install netlist-graph`.

The simulator itself never ships via PyPI; PyPI is the Python companion's
channel, not the GPU binary's. See [the Python-engine question](#the-python-engine-question)
below for why a wheel for the simulator is a live but undecided question of
its own.

## Why Linux isn't binstall-able

`cargo-binstall` selects a release asset by target triple. macOS/Metal has
one GPU backend for `aarch64-apple-darwin`, so the triple alone identifies
the right asset. Linux's `x86_64-unknown-linux-gnu` triple would have to
cover two backends, CUDA and HIP, that don't map to distinct triples: the
same triple can't resolve to two different binaries. There's no
target-triple axis to disambiguate on, so Linux installs stay a manual
choice: download the tarball for your backend, use a container, or build
from source.

## The GPU-backend feature matrix

Release and Homebrew binaries are built with `--features synth`, so the
[RTL on-ramp](rtl-onramp.md) works without a separate
synthesis step; a binary built without `synth` still simulates pre-synthesized
gate-level netlists and gives an actionable error on behavioral input. GPU
backend and `synth` are orthogonal features — `metal`, `cuda`, and `hip` each
select a backend, `synth` is additive on top of any of them.

Today only the Metal target has a release build. CUDA and HIP release
binaries are gated on self-hosted NVIDIA/AMD release runners; until those
land, CUDA/HIP users build from source (`cargo build -r --features
cuda,synth` / `--features hip,synth`).

## The Python-engine question

[Decision 0020](decisions/0020-python-engine-binary-wheel.md) drafts a binary
wheel that embeds the compiled `jacquard` binary, so `pip install jacquard`
would yield a working simulator with no separate install step. That decision
is **Draft, not ratified**. Its own direction note prefers a native PyO3
binding over the subprocess-bundled wheel it worked out in detail: an
in-process engine is a materially better Python surface, and building
per-platform wheels (cibuildwheel + dylib repair) is the same hard problem
either way, so it's worth doing once for a PyO3 extension rather than
building the subprocess-wheel machinery first and replacing it later. Which
of the two ships first is an open question tracked in
[issue #161](https://github.com/gpu-eda/Jacquard/issues/161). Nothing from
this decision is built.

## Constraints

- **No single universal binary.** Each GPU backend is a separate build
  (`metal` / `cuda` / `hip`); there is no fat binary spanning backends, and
  the release matrix has one row per target.
- **Linux is not binstall-able**, per [above](#why-linux-isnt-binstall-able).
  The release tarball, a container, or a source build are the only paths.
- **The `synth` feature must be on in released binaries.** It's what makes
  the behavioral [RTL on-ramp](rtl-onramp.md) (`jacquard sim design.v …`)
  work without a separate synthesis invocation; a binary without it only
  accepts gate-level netlists.
- **The Metal binary needs Homebrew LLVM at runtime.** It links Homebrew
  LLVM's `libc++` and `libomp`, since the mt-kahypar partitioner uses OpenMP
  via LLVM clang. The raw tarball and a `cargo binstall` install both ship
  the bare binary, which fails to launch without `brew install llvm` first.
  The Homebrew formula handles this itself via `depends_on "llvm"`.
- **`opensta-to-ir` has no independent release channel.** It ships inside
  the same tarball/formula as `jacquard`; there's no scenario where you get
  one without the other from the release channels.

## Implementation status

Built and in use: GitHub Releases for macOS/Metal, `cargo binstall --git`,
the Homebrew tap (stable + prerelease), staged install validation
(`validate-install.yml`) against a published (pre)release, `netlist-graph` on
PyPI via OIDC trusted publishing, single shared crate versioning via
`scripts/bump_version.py`, and the relocatable (embedded-metallib) Metal
binary that makes any of this possible. These are the sections above without
a "not yet."

Decided but not yet built:

- **CUDA/HIP prebuilt release binaries** — Decision 0018 Phase 4, gated on
  self-hosted NVIDIA/AMD release runners; Linux stays source-build until
  then. Phasing: [`../plans/distribution.md`](../plans/distribution.md).
- **Container image as a channel** — Decision 0018 Phase 6, deferred by the
  original decision rather than scheduled.
- **`eda-infra-rs` upstreaming**, the path to a crates.io publish and a plain
  `cargo install` — Decision 0018 Phase 7.
  [`../plans/distribution.md`](../plans/distribution.md) § Phase 7.
- **The Python engine wheel** (subprocess-bundled or PyO3) — Decision 0020,
  Draft and not ratified; the subprocess-vs-PyO3 choice is open, tracked in
  [issue #161](https://github.com/gpu-eda/Jacquard/issues/161) and
  [`../plans/python-engine-binary-wheel.md`](../plans/python-engine-binary-wheel.md).

## Decisions behind this

- [Decision 0018](decisions/0018-distribution-and-installation.md) —
  distribution and installation model: the channel-per-artifact split, why
  Linux isn't binstall-able, the Homebrew tap, and the still-open CUDA/HIP
  and container phases.
- [Decision 0020](decisions/0020-python-engine-binary-wheel.md) — Python
  engine as a bundled binary wheel: Draft, not ratified; subprocess-wheel vs.
  PyO3 is an open follow-on decision.
