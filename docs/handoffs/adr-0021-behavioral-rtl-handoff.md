# Handoff — ADR 0021 behavioral-RTL on-ramp (folded into `sim`/`cosim`)

**Updated:** 2026-07-04
**Branch:** `feat/rtl-onramp-build` (PR [#167](https://github.com/gpu-eda/Jacquard/pull/167), draft)
**Working tree:** Phase 3 doc changes uncommitted (human review requested); code is clean

## Goal & next-up

**Goal:** Behavioral Verilog / SystemVerilog becomes a first-class one-command input —
`jacquard sim design.v in.vcd out.vcd N` detects behavioral RTL, synthesizes it
transparently via embedded YoWASP Yosys, and simulates. No `build` step, no Python,
no external toolchain.

**Phases 1 and 2 are done and committed on this branch** (CI green). Phase 3
(documentation) is in progress — the changes from this session are uncommitted
and in the working tree for human review.

**Next session should pick up (priority order):**

1. **Review and commit Phase 3 doc changes.** Files changed this session:
   `docs/accepted-rtl.md` (new), `docs/getting-started.md`, `docs/installation.md`,
   `CLAUDE.md`, `docs/synthesis-flow.md`, `docs/plans/cell-model-ir.md`,
   this handoff. Human review first; then commit.
2. **Un-draft PR #167** and request review / merge. Nothing blocking; CI is green
   on the branch.
3. **Phase 4 follow-ups** (not gating, tracked in `docs/plans/rtl-onramp-sim-integration.md`):
   - Fetch `yosys.wasm` from GitHub release asset automatically on first use (ADR 0018 / #162 increment 2).
   - `--synth-target sky130|gf180` — synthesize to a real PDK for timing-accurate on-ramp runs.
   - ~~Wire `TimingLibrary::from_cell_model_ir` onto the runtime timing path~~ — **landed on `main`** (`de8255f3`, ADR 0019 D5); `--corner` on-ramp timing for built-in PDKs is now live.
   - Empirical SV/Verilog coverage table via sv-tests — makes `docs/accepted-rtl.md` authoritative rather than prose.

**Verification (Phases 1+2, already committed):**
```sh
# RTL synthesis + simulation (needs yosys.wasm):
JACQUARD_YOSYS_WASM=/path/to/yosys.wasm \
  cargo run -r --features metal,synth --bin jacquard -- \
    sim tests/counter/counter.v tests/counter/in.vcd /tmp/out.vcd 1
# Expect: "behavioral RTL → synthesized [YoWASP Yosys, functional QoR]"

# Confirm no `build` subcommand:
cargo run -r --bin jacquard -- build 2>&1 | grep -i "unrecognized\|error" || true

# CI:
gh pr checks 167
```

## Done this session (Phases 1 + 2, committed)

| What | Where |
|---|---|
| ADR 0021 amended: no standalone `build` command; synthesis folds into `sim`/`cosim`; Rust+wasmtime decision recorded | `docs/adr/0021-behavioral-rtl-support.md` |
| `Build` subcommand removed; `resolve_netlist_input` classifier added | `src/bin/jacquard.rs`, `src/sim/setup.rs` |
| `--rtl`, `--netlist`, `--emit-synth` flags added to `SimArgs` and `CosimArgs` | `src/bin/jacquard.rs` |
| CI `--features synth` build + behavioral smoke test added; `synth` added to `release.yml` / `user-acceptance.yml` (Phase 2) | `.github/workflows/` |
| Phase 3 doc changes (not yet committed — this session) | `docs/accepted-rtl.md` (new), `docs/getting-started.md`, `docs/installation.md`, `CLAUDE.md`, `docs/synthesis-flow.md`, `docs/plans/cell-model-ir.md` |

Earlier session (initial implementation — `src/synth.rs` engine):

| What | Where |
|---|---|
| Embedded Yosys synthesis engine (wasmtime, wasm caching, `read_slang` probe) | `src/synth.rs` |
| Synthesis script: slang/read_verilog → memory → assertions → aigpdk | `src/synth.rs::synth_script` |
| Spike preserved (reference) | `docs/spikes/rust-wasmtime-yosys/` |
| Validated: counter, assert_test, mem_test synthesize to aigpdk — zero leftover `$`-cells | tests/ |

## Critical context

**RTL detection heuristic.** `sverilogparse` is a structural-only parser;
behavioral constructs (`always`, arithmetic/logic operators in `assign`,
`if`/`case`) cause parse failure, which routes to synthesis. Pure net-alias
`assign`s and `wire` decls parse structurally (they appear in gate-level too)
and do not trigger synthesis. Use `--rtl` to force synthesis if detection
misclassifies; use `--netlist` to force direct gate-level loading.

**Wasm sourcing.** `sim`/`cosim` locate `yosys.wasm` in priority order:
`--yosys-wasm <path>` flag → `JACQUARD_YOSYS_WASM` env var → installed
`yowasp-yosys` Python package (`pip install yowasp-yosys`). The `--yosys-wasm`
flag was added this session (Phase 3 review) — the field was already plumbed
(`InputDispatch.yosys_wasm` → `SynthOptions` → `locate_yosys_wasm`) but the two
`cmd_sim`/`cmd_cosim` construction sites hardcoded `None`; they now read
`args.yosys_wasm`. The `src/synth.rs` error message ("Pass `--yosys-wasm <path>`,
set `JACQUARD_YOSYS_WASM`, …") is now accurate. Fetch-from-release is a Phase 4
item.

**`read_slang` (yosys-slang).** Probed once at startup via `help read_slang`.
Pinned wasm `yowasp-yosys 0.64.0.0.post1131` includes it (verified: `read_slang`
+ 495 `slang` symbols in the 39 MB module). Older wasm modules fall back to
`read_verilog -sv` gracefully.

**Synthesis script.** `read_slang|read_verilog -sv` → `hierarchy` → flatten +
proc + memory → `memory_libmap` (memlib_yosys.txt, RAMGEM) → `synth -run coarse:`
→ `techmap -map gem_formal.v` (maps `$check`/`$assert`/`$assume` → `GEM_ASSERT`,
`$print`/`$display` → `GEM_DISPLAY`, `$cover` → silently dropped) → dfflibmap +
abc (aigpdk_nomem.lib) → `write_verilog gatelevel.gv`. Techmap rules verified in
`aigpdk/gem_formal.v`.

**Wasm caching.** Compiled WASM module cached under `$XDG_CACHE_HOME/jacquard`
by content hash; first run pays ~20 s cranelift compile. Debug builds are ~50×
slower — always use `--release` for testing. The synthesized netlist is also
cached (keyed by design source + script + wasm hash), so repeat `sim` runs skip
synthesis.

**`dump-paths`, not `map`.** There is no `map` subcommand. To validate that a
netlist parses and inspect its timing paths (no GPU needed), use
`jacquard dump-paths <netlist> --liberty aigpdk/aigpdk.lib`.

**Actual error messages from `src/sim/setup.rs`:**
- Unknown cells (gate-level, not synthesized): `"gate-level netlist with unrecognized cell type(s): {cells}. This is already a netlist (not behavioral RTL)… Pass \`--cell-descriptor <path>\` (ADR 0019)…"`
- Both structural parse and synthesis fail: `"could not load as a gate-level netlist, and synthesizing it as behavioral RTL also failed"` (both errors surfaced).
- Synth path but binary built without `--features synth`: `"input looks like behavioral RTL, but this jacquard binary was built without synthesis support. Rebuild with \`--features synth\`…"`
- Success log line: `"{}: behavioral RTL → synthesized [YoWASP Yosys, functional QoR] → {}"`

**abc is compiled in-tree into `yosys.wasm`, called in-process** — WASI has no
`exec`. This matters for the ADR 0021 Phase 2 provenance spike: whether `\src`
data survives the in-process abc path is unproven and is the first thing to spike.

## ADR 0021 Phase 2 — `\src` provenance (unchanged, separate)

Fork [YoWASP/yosys](https://codeberg.org/YoWASP/yosys) (GitHub mirror archived
2026-03-11), repoint `yosys-src` → `robtaylor/yosys@src-retention-y-ext` and
bundled abc submodule → `robtaylor/abc@origin-tracking-clean` (abc#487), rerun
`build.sh` (wasi-sdk 27). First task = spike whether `\src` survives in-process
abc before committing to the WASM build effort.

## References

- [ADR 0021](../adr/0021-behavioral-rtl-support.md) — the decision (Decision §1 dispatch table, Consequences accepted-RTL surface)
- [Plan: RTL on-ramp sim integration](../plans/rtl-onramp-sim-integration.md) — Phase 4 roadmap
- [Accepted RTL surface](../accepted-rtl.md) — user-facing doc (new, Phase 3)
- [#162](https://github.com/gpu-eda/Jacquard/issues/162) · PR [#167](https://github.com/gpu-eda/Jacquard/pull/167)
- Assertion techmap: `aigpdk/gem_formal.v`; GEM_ASSERT: `aigpdk/aigpdk.v:174`, `src/aigpdk.rs:52,91`
- Spike reference: `docs/spikes/rust-wasmtime-yosys/`

---

**Resume in a new session with:**
```
/resume_handoff docs/handoffs/adr-0021-behavioral-rtl-handoff.md
```
