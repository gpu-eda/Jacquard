# Handoff — ADR 0021 behavioral-RTL on-ramp (`jacquard build` via YoWASP)

**Created:** 2026-07-01
**Working tree:** clean
**Branch:** main

## Goal & next-up

**Goal of this session:** clarify Jacquard's input story (external feedback: "not
clear what netlist language you accept") and record the decision to make
behavioral RTL first-class. Shipped the docs + [ADR 0021](../adr/0021-behavioral-rtl-support.md)
(Proposed) in PR #163; filed the implementation tracker
[#162](https://github.com/gpu-eda/Jacquard/issues/162). **No implementation
started** — the ADR is ratified as *Proposed*, the code is the next arc.

**Next session should pick up:** **Phase 1** of #162 — implement
**`jacquard build <design.v>`**: shell out to Yosys (via `yowasp-yosys`) running
the *existing* synthesis scripts, producing `gatelevel.gv`, then hand off to
`sim`. The scripts already exist — memory synthesis uses `aigpdk/memlib_yosys.txt`
(`memory_libmap`), logic synthesis maps to `aigpdk/aigpdk.lib` cells; the full
manual flow is [`docs/synthesis-flow.md`](../synthesis-flow.md). Start by wiring
`yowasp-yosys` (already resolved — see below) to run those two steps.

**Verification command:**
```sh
# ADR + docs are on main:
ls docs/adr/0021-behavioral-rtl-support.md docs/input-netlist.md
grep -n "0021" docs/adr/README.md docs/SUMMARY.md
uv run python scripts/check_doc_links.py     # Expect: "All rendered-page links resolve"
# YoWASP Yosys is already a resolved dependency:
grep -n "yowasp-yosys" uv.lock                # Expect: present (via amaranth[builtin-yosys])
```

## Done this session

| PR / issue | What |
|---|---|
| **#163** (merged, `559f4766`) | README `## Input`; `docs/input-netlist.md` (full structural-Verilog subset + SVA status); ADR 0021 (Proposed) |
| **#162** (open) | Implementation tracker — Phase 1 (`jacquard build`/YoWASP) + Phase 2 (`\src` provenance) |

## Open follow-ups (priority order)

### 1. Phase 1 — `jacquard build` via stock YoWASP (the on-ramp)
Drive `yowasp-yosys` through `aigpdk/memlib_yosys.txt` + `aigpdk/aigpdk.lib`
synthesis → `gatelevel.gv` → `sim`. Micro-decisions still open (see ADR 0021 /
#162): command name (**`build`** chosen) vs `--rtl` flag; **Rust subcommand vs
Python entry** (leaning Python, since YoWASP is a Python package and this homes in
the Python engine — #161); how `--top-module` and clock-gating (`CKLNQD`) config
surface. Back-end should be abstracted so it can dispatch to YoWASP wheel · Nix
devshell · system Yosys/DC.

### 2. Phase 2 — `\src` provenance (the moat), gated on upstreaming
Patched (self-built) YoWASP carrying origin-shell's source pass-through → thread
`\src` through `netlistdb`/AIG so `--trace-signals` / timing / X-debug speak RTL.
**Blocked on** berkeley-abc **#487** landing (in review) + a patched-YoWASP or Nix
build. Not startable until #487 is in.

## Critical context

- **YoWASP is already in `uv.lock`** (transitively via `amaranth[builtin-yosys]`
  in `designs/mcu_soc_sky130/`), so Phase 1 adds *no new heavy dependency* — the
  WASM Yosys wheel is already resolved for the workspace.
- **QoR split is load-bearing:** YoWASP Yosys is functional-grade; **bring-your-own
  DC stays the performance path** (synthesis quality sets Jacquard's GPU speed,
  per `synthesis-flow.md`). Don't let `jacquard build` be mistaken for the fast
  path — document both.
- **origin-shell PoC** (for Phase 2) lives at `~/Code/ChipFlow/reference/origin-shell`
  (GitHub `robtaylor/origin-shell`) — a Nix flake pinning patched abc (#487) +
  yosys (`src-retention-y-ext`) + librelane (`SYNTH_ABC_BACKEND=origins`). It
  keeps `\src` alive through std-cell mapping at ~100% coverage. Building *our
  own* YoWASP lets us compile the patched abc to WASM alongside yosys, so
  provenance can ship self-contained in one wheel.
- **Emulator core untouched:** `jacquard build` is a *pre-processor* producing the
  same structural netlist users synthesize by hand today. The AIG/boomerang
  pipeline (ADR 0014/0015) and the structural `sverilogparse` input don't change.
- **Home tie:** this lands in the Python engine, whose *packaging* decision
  (subprocess wheel vs PyO3) is itself deferred — ADR 0020 is **Draft**, tracked
  in [#161](https://github.com/gpu-eda/Jacquard/issues/161) (PyO3 leaning). Phase 1
  doesn't need that resolved, but the two share the Python layer.

## References

- [ADR 0021](../adr/0021-behavioral-rtl-support.md) — the decision (+ Nix
  alternative, Phase-2 provenance).
- [#162](https://github.com/gpu-eda/Jacquard/issues/162) — implementation tracker.
- [`docs/synthesis-flow.md`](../synthesis-flow.md) — the manual flow `jacquard build` wraps.
- [`docs/input-netlist.md`](../input-netlist.md) — what the netlist input accepts today.
- [ADR 0014](../adr/0014-aig-as-simulation-ir.md) — why synthesis is a front-end.
- #161 / ADR 0020 — the Python engine that hosts this. berkeley-abc #487 — the Phase-2 gate.

---

**Resume in a new session with:**
```
/resume_handoff docs/handoffs/adr-0021-behavioral-rtl-handoff.md
```
