# Handoff — ADR 0021 behavioral-RTL on-ramp (`jacquard build`)

**Updated:** 2026-07-03
**Branch:** `feat/rtl-onramp-build` (PR [#167](https://github.com/gpu-eda/Jacquard/pull/167), **draft**, CI green)
**Working tree:** clean (only `.env` untracked)

## Goal & next-up

**This session:** implemented **Phase 1** of ADR 0021 / [#162](https://github.com/gpu-eda/Jacquard/issues/162)
— `jacquard build <design.v…>` synthesizes behavioral RTL → gate-level aigpdk
netlist that feeds `sim`/`cosim`, with **no Python and no external toolchain**.
Shipped on PR #167 (draft, CI green on the branch and on `main`). The ADR was
amended to record the **Rust + `wasmtime`** shape (was "Python-hosted").

**Next session should pick up** (priority order):
1. **Un-draft PR #167** and get it reviewed/merged (nothing blocking; CI green).
2. **CI coverage for the `synth` feature** — it's opt-in (not default), so current
   CI (`cargo test --lib`, `cargo build -r --bin jacquard`) never compiles it. Add
   a `cargo build --features synth` job + a `jacquard build` smoke test, and add
   `--features synth` to `release.yml` / `user-acceptance.yml` so shipped binaries
   include the on-ramp. **Without this the feature ships in no binary.**
3. **Fetch-from-GitHub-release** (increment 2): publish the pinned `yosys.wasm` as
   a Jacquard release asset; `build` fetches to `$XDG_CACHE_HOME/jacquard` + sha256.
   Same mechanism will later deliver the Phase-2 patched wasm. Aligns with ADR 0018.

**Verification:**
```sh
# feature builds + runs (needs a yosys.wasm; e.g. from an installed yowasp-yosys wheel):
cargo build --release --features synth --bin jacquard
JACQUARD_YOSYS_WASM=<path/to/yosys.wasm> \
  ./target/release/jacquard build <design.v> -o /tmp/gl.gv     # → aigpdk netlist
./target/release/jacquard dump-paths /tmp/gl.gv --liberty aigpdk/aigpdk.lib   # parses → AIG
cargo build --bin jacquard        # default build: `build` cleanly absent (feature-gated)
gh pr checks 167
```

## Done this session

| What | Where |
|---|---|
| ADR 0021 amended: Rust+wasmtime decision, abc-in-tree/in-process finding, Codeberg fork recipe, in-process-`\src` risk, Python-hosted alt rejected | `docs/adr/0021-behavioral-rtl-support.md` |
| Spike preserved (reference) | `docs/spikes/rust-wasmtime-yosys/` |
| `jacquard build` implementation | `src/synth.rs` (new), `src/bin/jacquard.rs` (`Build` subcmd, `cmd_build`), `src/lib.rs`, `Cargo.toml` (`synth` feature + wasmtime/anyhow/log deps) |
| Repo-wide `cargo fmt` | separate commit on **`main`** (`49aa9cff`); branch rebased on it |
| fmt-before-commit hook (needs `/hooks` reload to activate) | `.claude/settings.json` (gitignored) |

Validated: **counter** (logic+DFF), **assert_test** (→ `GEM_ASSERT`), **mem_test**
(→ `RAMGEM`) all synthesize to aigpdk cells, zero leftover `$`-cells; outputs
parse + build an AIG via `dump-paths`.

## Critical context / decisions

- **Rust + wasmtime, not Python.** `yowasp_runtime` is a ~100-line `wasmtime`+WASI
  harness; `src/synth.rs` ports it. `build` is a clap subcommand behind the opt-in
  **`synth` feature** (wasmtime+cranelift are heavy to compile). Decoupled from the
  #161 Python-engine decision.
- **abc is compiled in-tree into `yosys.wasm`, called in-process** (WASI has no
  `exec`) — verified from the shipped module. This de-risks Phase 2: the only
  remaining `\src`-provenance unknown is whether `&origins` data survives the
  *in-process* abc path (origin-shell only validated the **external**-abc path).
- **Synthesis script** (`src/synth.rs::synth_script`): `read_verilog -sv` → flatten
  → `memory_libmap` (memlib_yosys.txt) → `synth -run coarse:` → **`techmap -map
  gem_formal.v`** (assertions → `GEM_ASSERT`; `--strip-assertions` uses `chformal
  -remove`) → dfflibmap/abc to aigpdk. Support files embedded via `include_str!`.
  Do **not** `read_verilog -lib aigpdk.v` — it fails to parse (`aigpdk.v:64`); cells
  emit fine as blackboxes.
- **wasm compile is cached** to `$XDG_CACHE_HOME/jacquard` by content hash
  (`load_module`), so only the first run pays the (large, ~20 s) cranelift compile.
  **Debug builds are ~50× slower to compile the wasm** — always test with
  `--release`. cranelift's DEBUG log torrent is clipped to Info around the compile.
- **wasm sourcing:** now via `--yosys-wasm` / `JACQUARD_YOSYS_WASM` / installed
  `yowasp-yosys` discovery. Chosen fetch default = **GitHub release asset** (above).
- **No `map` subcommand exists** (CLAUDE.md/docs references are stale) — use
  `dump-paths` (no GPU) to validate a netlist parses.
- **Non-synthesizable constructs:** testbench-only (`$display`, delays) dropped by
  synth; immediate assertions → `GEM_ASSERT`; concurrent SVA limited by YoWASP
  Yosys (no Verific) — broader SVA is the #106/#107 roadmap.

## Phase 2 (roadmap, unchanged, not started)

`\src` provenance: fork [YoWASP/yosys](https://codeberg.org/YoWASP/yosys) (now on
Codeberg; GitHub mirror archived 2026-03-11), repoint `yosys-src` →
`robtaylor/yosys@src-retention-y-ext` and yosys's bundled `abc` submodule →
`robtaylor/abc@origin-tracking-clean` (abc#487), rerun `build.sh` (wasi-sdk 27).
**First task = spike whether `\src` survives in-process abc** before committing.

## Follow-ups not yet filed
- CI `--features synth` coverage (see next-up #2) — **most important**, feature
  ships in no binary until done.
- Stale `jacquard map` references in `CLAUDE.md` / `docs/`.
- Activate the fmt hook (`.claude/settings.json`) via `/hooks` reload.

## References
- [ADR 0021](../adr/0021-behavioral-rtl-support.md) · [#162](https://github.com/gpu-eda/Jacquard/issues/162) · PR [#167](https://github.com/gpu-eda/Jacquard/pull/167)
- [`docs/spikes/rust-wasmtime-yosys/`](../spikes/rust-wasmtime-yosys/) — the proving spike
- [`docs/synthesis-flow.md`](../synthesis-flow.md) · [`docs/input-netlist.md`](../input-netlist.md)
- Assertion techmap: `aigpdk/gem_formal.v`; GEM_ASSERT cell: `aigpdk/aigpdk.v:174`, `src/aigpdk.rs:52,91`

---
**Resume with:** `/resume_handoff docs/handoffs/adr-0021-behavioral-rtl-handoff.md`
