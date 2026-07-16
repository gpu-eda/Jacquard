# Development

For working on Jacquard itself. If you only want to *run* it, start at
[Installation](installation.md) and [Getting Started](getting-started.md).

## Build

You need the [Rust toolchain](https://rustup.rs/) (2021 edition) and the GPU SDK
for the backend you're building. Nothing else.

```sh
git submodule update --init --recursive
```

The GPU backend is a feature, and you pick exactly one:

```sh
cargo build -r --features metal --bin jacquard   # macOS, Apple Silicon
cargo build -r --features cuda  --bin jacquard   # NVIDIA, CUDA toolkit
cargo build -r --features hip   --bin jacquard   # AMD, ROCm
```

Two features are worth knowing about:

- **`synth`** adds the embedded YoWASP Yosys engine, which is what lets
  `sim`/`cosim` take behavioral RTL instead of a gate-level netlist (see
  [Accepted RTL Surface](accepted-rtl.md)). It costs real compile time —
  wasmtime and cranelift — so it is opt-in for local builds, and on for release
  binaries. Combine it with a backend: `--features metal,synth`.
- **No GPU feature at all** still builds `dump-paths`, so timing analysis and
  netlist validation work on a machine with no GPU SDK installed.

## Test

```sh
cargo test                       # library + integration tests, no GPU needed
cargo bench --bench event_buffer # criterion micro-benchmarks, no GPU needed
cargo bench --bench xprop
```

For the GPU path, two flags matter more than the test suite:

```sh
jacquard sim ... --check-with-cpu        # run a CPU baseline and compare
jacquard sim ... --max-clock-edges 1000  # bound a long run while bisecting
                                         # (edges, not cycles: 2 per cycle)
```

`--check-with-cpu` is the one to reach for when a kernel change produces
plausible-looking waveforms: it is the difference between "it ran" and "it is
right".

Benchmark designs (NVDLA, Rocket, Gemmini) live in `benchmarks/dataset/`, a
submodule. NVDLA is the smallest and the usual first thing to try.

## Where things live

| Path | What's in it |
|---|---|
| `src/aig.rs` | the and-inverter graph, and the conversion from NetlistDB into it |
| `src/staging.rs` | splits the AIG into pipeline stages (`--level-split`) |
| `src/repcut.rs` | hypergraph partitioning onto GPU blocks, via mt-kahypar |
| `src/pe.rs` | maps a partition onto one block's resources; the limits below live here |
| `src/flatten.rs` | emits `FlattenedScriptV1`, the packed instruction stream the kernel runs |
| `src/aigpdk.rs` | the AIGPDK standard-cell interface (AND, DFF, clock gate, SRAM) |
| `src/synth.rs` | the embedded Yosys on-ramp (ADR 0021), behind `synth` |
| `csrc/kernel_v1.metal` | the Metal kernel |
| `csrc/kernel_v1.cu`, `kernel_v1.hip.cpp` | CUDA and HIP, sharing `kernel_v1_impl.cuh` |
| `crates/` | `timing-ir`, `opensta-to-ir`, `cell-model-ir`, `liberty-parse`, `liberty-to-cellir`, `cell-decomp` |
| `vendor/eda-infra-rs` | netlistdb, sverilogparse, vcd-ng, ulib, ucc, clilog |
| `docs/scripts/` | documentation tooling, kept under `docs/` so editing it isn't a code change |

The pipeline reads left to right:

```
NetlistDB → AIG → StagedAIG → Partitions → FlattenedScript → GPU kernel
```

[Simulation Architecture](simulation-architecture.md) walks each stage. For
*why* it is shaped this way, [ADR 0014](adr/0014-aig-as-simulation-ir.md)
explains the AIG choice and [ADR 0015](adr/0015-boomerang-execution-model.md)
the boomerang execution model.

## The constraint that shapes everything

A GPU block is a fixed budget, and `src/pe.rs` has to fit each partition inside
it:

- at most **8191 unique inputs** and **8191 unique outputs** per partition
  (for SRAMs and DFFs, outputs count every enable and bus pin, and holes mean
  the effective capacity can be as low as 4095);
- at most **4095 intermediate pins alive** at any stage;
- at most **64 SRAM output groups** — that is 8192 / (32 × 4).

When a design doesn't fit you get `single endpoint cannot map`. The answer is
usually `--level-split`, which forces more stages so each one is smaller:

```sh
jacquard sim ... --level-split 30
jacquard sim ... --level-split 20,40
```

This is the first thing most people hit on a large design, and it is a mapping
limit rather than a bug.

## Optional tooling

Each of these is needed by one workflow and nothing else. Editing Rust, C++ or
kernel sources needs none of them: the timing-IR bindings are checked in, and
OpenSTA is only for the timing-correctness corpus.

| Tool | Used for | macOS (Homebrew) | Linux (Debian/Ubuntu) |
|---|---|---|---|
| `flatc` | regenerating timing-IR bindings when editing `crates/timing-ir/schemas/timing_ir.fbs` | `brew install flatbuffers` | `apt install flatbuffers-compiler` |
| `mdbook` | building the docs locally | `brew install mdbook` | `cargo install mdbook` |
| OpenSTA | building vendored `vendor/opensta/` for `opensta-to-ir` and the timing-correctness CI corpus | `brew bundle --file vendor/opensta/Brewfile`, then `scripts/build-opensta.sh` | see `vendor/opensta/Dockerfile.ubuntu22.04`, then `scripts/build-opensta.sh` |

Python tooling (PDK fetchers, harness utilities) belongs in the workspace's `uv`
dev group rather than an ad-hoc pip install: add it under `[dependency-groups]`
in the root `pyproject.toml`, `uv sync --group dev`, and run it with `uv run`.

## Debugging

Jacquard's own tools, each with a page:

- [Signal Tracing](signal-tracing.md) — surface internal nets in the output VCD.
  `netlist-graph` finds the names to trace: `uv run netlist-graph drivers <netlist> <signal>`.
- [Debugging X Values](x-debugging.md) — find *why* a signal went `x`, statically,
  instead of trace-guess-rerun.
- [Bus Transaction Tracing](bus-tracing.md) — decode on-chip bus transfers rather
  than reading raw wires.
- [Timing Violations](timing-violations.md) — GPU-side setup/hold checks.
- [Cosim Perf Report](cosim-perf-report.md) and [GPU Frame Capture](gpu-capture.md)
  — where the time actually goes in a cosim run.

## Docs

The docs are an mdBook, sourced from `docs/`:

```sh
mdbook serve   # http://localhost:3000
```

`SUMMARY.md` is the table of contents, and a page missing from it isn't rendered
at all — which is why `docs/scripts/check_doc_links.py` validates links against
the *rendered* page set rather than against files on disk. Run it before pushing
docs; CI does too.

The published site keeps main's docs at the root and a frozen copy of each
release under `/vX.Y.Z/`, with a picker to move between them. Release notes link
to the pinned copy, so a link in an old release keeps meaning what it meant. See
[Release Process](release-process.md).

## Conventions

- **[ADRs](adr/README.md)** record decisions worth understanding later, and are
  append-only: when reality moves past one, amend it rather than rewriting it.
  An ADR's status is a claim about the code, so check it against the code.
- **[Plans](plans/README.md)** hold work in progress; deferred work gets a plan
  and an issue rather than being lost.
- **[Handoffs](handoff-discipline.md)** are working memory, not history. One per
  active thread, folded into the ADRs or plans and deleted when resolved.
- **[Release Process](release-process.md)** covers cutting a release, the RC-first
  flow, and what's automated.

The through-line: a sentence that says how the tool behaves is a verifiable
claim. That applies to docs, `--help` text, and ADR status alike — check it
against the code before writing it.
