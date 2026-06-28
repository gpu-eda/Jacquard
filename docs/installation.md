# Installation

Jacquard is three tools; install only what your task needs:

| Tool | What it's for | Install |
|------|---------------|---------|
| **`jacquard`** | the simulator (`sim` / `cosim`) | Homebrew · `cargo binstall` · prebuilt release · from source |
| **`opensta-to-ir`** | SDF → timing-IR (only for the timing / post-PnR path) | ships **with `jacquard`** (same release / Homebrew formula) |
| **`netlist-graph`** | post-synthesis signal-name discovery (companion to the [tracing docs](signal-tracing.md)) | PyPI (`uvx` / `pip`) |

> **Availability.** The prebuilt-binary, Homebrew, and PyPI channels go
> live with the **first tagged release (`v0.1.0`)**. Until then, build the
> simulator [from source](#from-source-any-backend) and run `netlist-graph`
> from the repo with `uv run`. The design behind this layout is
> [ADR 0018](adr/0018-distribution-and-installation.md).

## The simulator (`jacquard` + `opensta-to-ir`)

### Homebrew — macOS / Apple Silicon (Metal)

```sh
brew install gpu-eda/tap/jacquard      # installs jacquard + opensta-to-ir
```

The cleanest path on a Mac. Requires an Apple Silicon machine with a
Metal GPU.

### cargo binstall — prebuilt binary, no toolchain build

```sh
brew install llvm     # runtime dependency — see note below
cargo binstall --git https://github.com/gpu-eda/Jacquard jacquard
```

Fetches the release binaries (`jacquard` + `timing_analysis`) on
**macOS/Metal**. The `--git` form is required: `jacquard` is **not on
crates.io** (its dependencies are a vendored fork carrying in-flight
patches), so binstall reads the `[package.metadata.binstall]` pkg-url
straight from the repo rather than looking the crate up on the registry.
Linux is **not** binstall-able: there are two GPU backends (CUDA, HIP) for
one target triple, so it can't be auto-selected — use the release tarball
for your backend, a container, or build from source.

> **Runtime dependency — Homebrew LLVM.** The prebuilt macOS binary links
> Homebrew LLVM's `libc++` and `libomp` (the build uses LLVM clang for
> OpenMP, via the mt-kahypar partitioner), so it needs `brew install llvm`
> to run. The **Homebrew** install handles this automatically
> (`depends_on "llvm"`); **binstall** and the **raw tarball** do not, so
> install LLVM first.

### Prebuilt release tarball

Download `jacquard-<version>-<target>.tar.gz` from the
[releases page](https://github.com/gpu-eda/Jacquard/releases), extract,
and put `jacquard`, `timing_analysis`, and `opensta-to-ir` on your `PATH`.
The GPU kernel is embedded, but the binary still needs Homebrew LLVM at
runtime (`brew install llvm`) — see the note above.

### From source (any backend)

The portable path, and the only one for **Linux CUDA / HIP** today.
Needs the [Rust toolchain](https://rustup.rs/) and the GPU SDK for your
backend.

```sh
git clone https://github.com/gpu-eda/Jacquard.git
cd Jacquard
git submodule update --init --recursive

cargo build -r --features metal --bin jacquard   # macOS / Apple Silicon
cargo build -r --features cuda  --bin jacquard   # NVIDIA (CUDA toolkit)
cargo build -r --features hip   --bin jacquard   # AMD (ROCm)
```

The binary lands at `target/release/jacquard`. See the README's
*Dependencies* table for optional tooling (`flatc`, `mdbook`, OpenSTA).

## The signal-analysis companion (`netlist-graph`)

Pure Python — install from PyPI, no GPU or Rust needed:

```sh
uvx netlist-graph search design.gv psel     # one-off, no install
pip install netlist-graph                    # or install it
```

From a Jacquard checkout you can also run it without installing:
`uv run netlist-graph …` (it's a workspace member). See
[signal tracing](signal-tracing.md) for what it's used for.

## The timing path (`opensta-to-ir` + a PDK)

For post-PnR timing simulation you also need PDK Liberty files, fetched
with [volare/ciel](https://github.com/fossi-foundation/ciel) (pinned in
the root `pyproject.toml`). `opensta-to-ir` converts SDF to the Jacquard
timing IR (`.jtir`) that `jacquard sim --timing-ir` / `cosim --timing-ir`
consume. Pure functional (pre-PnR) runs need none of this — see
[signal tracing § pre-PnR functional runs](signal-tracing.md#pre-pnr-functional-runs).

## Verify

```sh
jacquard --version
# A quick self-contained cosim (from a Jacquard checkout):
jacquard cosim tests/apb_trace/apb_trace_synth.gv \
    --config tests/apb_trace/sim_config.json \
    --top-module apb_trace --max-clock-edges 200 \
    --bus-trace-csv /tmp/apb.csv
```

Then head to [Getting Started](getting-started.md) to run bundled designs, or the
[Synthesis Flow](synthesis-flow.md) to prepare your own RTL.
