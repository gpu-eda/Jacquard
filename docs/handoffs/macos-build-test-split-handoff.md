# Handoff: macOS Metal build/test split

**Goal:** relieve the `macos-runner-1` CI bottleneck by splitting the Metal jobs
the same way the CUDA/HIP jobs were split (PR #152) — compile once on a free,
scaling GitHub-hosted macOS runner and run the tests (which need the Metal GPU)
on the runner(s), instead of every Metal job compiling on the single serial
self-hosted box.

Unlike the CUDA split (which saved **money**), this one is about **latency**:
`macos-runner-1` is one serial machine, so moving the compile off it is what
unclogs the macOS queue that every PR waits behind.

---

## ⚠️ First: watch #151

#151 (Blackwell runner + `cuda-blackwell` full-suite + shared
`scripts/ci/gpu_test_suite.sh`) was **merged to main** at the end of the
session. Its merge triggered a push-CI run on `main`. **Confirm that run went
green** before starting new work:

```sh
gh run list --branch main --limit 3 --json databaseId,status,conclusion,headSha
```

The full CI stack that landed this session (all on `main`): #149 (arch flag),
#148 (docs deploy), **#152** (CUDA+HIP build/test split), **#151** (Blackwell
runner + shared GPU suite). If main is red, that's the first thing to fix.

---

## What's already validated (don't re-investigate)

- **The metallib is embedded in the binary** via `include_bytes!(env!("METALLIB_PATH"))`
  — `src/bin/jacquard.rs:941` and `src/sim/cosim/metal.rs:154`. So a
  Metal-built `jacquard` binary is **self-contained**; the split artifact is
  *just the binary*, no separate `.metallib` to ship.
- **`macos-latest` is free** for this public repo (billing usage shows
  `Actions macOS 3-core: $0`), Apple-Silicon (arm64, Metal-capable), scales ~×5.
  Use it for the build job. No custom runner needed (a `macos-build` hosted
  runner was briefly added then deemed redundant — can be removed).
- **Deployment target** is `macos_version_min("14.0")` (`build.rs:141`), so a
  binary built on `macos-latest` runs on any macOS 14+ test runner
  (`macos-runner-1` is Apple Silicon, almost certainly ≥14). Validate in CI.
- **Runtime libs (the real wrinkle):** the Metal build uses **Homebrew LLVM**
  (`CC/CXX = llvm/bin/clang`) and links Homebrew **libc++** (`LIBRARY_PATH=
  ${LLVM_PREFIX}/lib/c++`) plus **libomp** (OpenMP). So the *test* runner needs
  those dylibs at runtime → `brew install llvm` (or `libomp` + `libc++`). On the
  **persistent** `macos-runner-1` this is already installed (fast/cached); keep
  the existing "Install LLVM" step there, just relabel it "runtime libs".
- **actionlint:** `macos-latest` is a built-in label — **no** `.github/actionlint.yaml`
  change needed (unlike the custom blackwell labels).

## The design (proven; ready to implement)

Add one build job, then convert each test job to download+run. Keep the
required-check *names* on the test jobs (`Metal Tests (macOS)`, `MCU SoC Metal
Simulation`) so branch protection is untouched.

`metal-build` job (this exact shape validated as a job definition):

```yaml
  metal-build:
    needs: [changes]
    if: needs.changes.outputs.code == 'true'
    name: Metal Build
    runs-on: macos-latest
    steps:
      - uses: actions/checkout@v6
        with: { submodules: false }
      - name: Init required submodules
        run: git submodule update --init vendor/eda-infra-rs vendor/sky130_fd_sc_hd
      - name: Install Rust
        uses: dtolnay/rust-toolchain@stable
      - name: Install LLVM (clang + OpenMP for the Metal build)
        run: |
          brew install llvm          # hosted runner: brew already on PATH (no shellenv eval)
          LLVM_PREFIX=$(brew --prefix llvm)
          echo "CC=${LLVM_PREFIX}/bin/clang"  >> "$GITHUB_ENV"
          echo "CXX=${LLVM_PREFIX}/bin/clang++" >> "$GITHUB_ENV"
          echo "LIBRARY_PATH=${LLVM_PREFIX}/lib/c++:${LLVM_PREFIX}/lib" >> "$GITHUB_ENV"
          LLVM_VER=$(${LLVM_PREFIX}/bin/clang --version | head -1 | grep -o '[0-9]*\.[0-9]*\.[0-9]*')
          echo "LLVM_VERSION=$LLVM_VER" >> "$GITHUB_ENV"
      - name: Cache cargo
        uses: actions/cache@v5
        with:
          path: |
            ~/.cargo/bin/
            ~/.cargo/registry/index/
            ~/.cargo/registry/cache/
            ~/.cargo/git/db/
            target/
          key: ${{ runner.os }}-cargo-metal-build-llvm${{ env.LLVM_VERSION }}-${{ hashFiles('**/Cargo.lock') }}
          restore-keys: ${{ runner.os }}-cargo-metal-build-llvm${{ env.LLVM_VERSION }}-
      - name: Build jacquard (Metal)
        run: cargo build --release --features metal --bin jacquard
      - name: Upload jacquard-metal binary
        uses: actions/upload-artifact@v7
        with: { name: jacquard-metal, path: target/release/jacquard, retention-days: 1 }
```

Per test job (`metal`, `mcu-soc-metal`, `jtag-minimal-cosim`,
`jtag-minimal-cosim-server`, `jtag-minimal-openocd`):
1. `needs:` += `metal-build`.
2. Required ones (`metal`, `mcu-soc-metal`): `if: always() && needs.changes.outputs.code == 'true'`
   so a failed build **fails** the required check instead of skipping (a skipped
   required check counts as passing). The `Download` step failing on the missing
   artifact is the de-facto gate.
3. Keep the existing "Install LLVM" step (now for **runtime** libs).
4. Add:
   ```yaml
   - name: Download jacquard (Metal)
     uses: actions/download-artifact@v8
     with: { name: jacquard-metal, path: target/release }
   - run: chmod +x target/release/jacquard
   ```
5. Remove that job's local `cargo build --release --features metal --bin jacquard` (if it has one).
6. Convert `cargo run --release --features metal --bin jacquard -- …` →
   `target/release/jacquard …`.

## ‼️ The two mistakes I made — do NOT repeat

1. **Do NOT use a file-wide `replace_all`** for the `cargo run` → binary swap.
   The macOS jobs are **heterogeneous**:
   - `metal`, `jtag-minimal-cosim-server`, `jtag-minimal-openocd` → have an
     explicit `cargo build --features metal`.
   - **`mcu-soc-metal` and `jtag-minimal-cosim` have NO explicit build** — they
     compiled implicitly via `cargo run`. A file-wide replace converted them to
     run a binary *nothing produces* → broken. Also `mcu-soc-metal` is complex
     (firmware build + jtir download via `prepare-mcu-soc-jtir` + `uv`).
   → Edit **one job at a time**, scoped.
2. **Trailing-space bug:** the string is `cargo run --release --features metal --bin jacquard -- ` (trailing space before `sim`/`cosim`). Replacing it with
   `target/release/jacquard` (no trailing space) glued the words →
   `target/release/jacquardsim`. Replace with `target/release/jacquard ` **including
   the trailing space**, or drop the `-- ` in the match. Verify after:
   `grep -n 'jacquard\(sim\|cosim\)' .github/workflows/ci.yml` must be empty.

## Recommended plan

Incremental, so the runtime-lib story is validated before touching all 5 jobs:
1. Branch `ci/split-macos-build-test` (**already exists, reset to clean main** —
   reuse it). Add `metal-build` + split **only** the `metal` (Metal Tests) job.
2. PR it, watch **Metal Build** (macos-latest) and **Metal Tests** (macos-runner-1):
   does the hosted-built binary run on the self-hosted box with `libomp`/`libc++`,
   and is the deployment target OK? This is the whole risk.
3. If green, extend to `jtag-minimal-cosim-server` + `jtag-minimal-openocd` (they
   already build explicitly — straightforward), then `jtag-minimal-cosim`, then
   `mcu-soc-metal` (most involved) — one commit each.

Note the test jobs keep the `runs-on: ${{ …xlarge on main/label… || 'macos-runner-1' }}`
conditional; the build job is plain `macos-latest`. `python3` is already present
on `macos-runner-1` (used by the xprop_cosim checks); the build job needs none.

## Context / state at handoff time

- CUDA/HIP build/test split, the shared `scripts/ci/gpu_test_suite.sh`, and the
  Blackwell runner are all **on main**.
- The **Blackwell self-hosted runner is live** (org-level on gpu-eda, ephemeral
  container, labels `[self-hosted, cuda, blackwell, sm_120]`), running the full
  suite via `cuda-blackwell`. Setup + ops: `ci/blackwell-runner/README.md`.
- Fork-PR gating was deliberately **omitted** (container isolation + dedicated
  open-source box); tighten with `all_external_contributors` approval later if
  wanted.

> **Handoff discipline note:** two other handoffs (`cell-model-ir`,
> `issue-triage-and-upstreaming`) currently coexist with this one, against the
> "exactly one at a time" rule in `CLAUDE.md`. Reconcile/remove resolved ones.
