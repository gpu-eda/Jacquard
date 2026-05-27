# Handoff — Release prep: NVIDIA runner and remaining blockers

**Created:** 2026-05-27
**Working tree:** clean
**Branch:** main

## Goal & next-up

**Goal of this session:** Ship the `--timing-report` CI test (release checklist item), implement multi-UART cosim (#90), move repo to `gpu-eda` org, and document undocumented architecture in ADRs.

**Next session should pick up:** Register the NVIDIA runner in the `gpu-eda` org, re-enable CUDA CI, then work through remaining release blockers.

**Verification command:**
```sh
gh run list --limit 1 --json conclusion,headSha
# Expect: conclusion=success, headSha=dfae6ee
```

## Done this session

| Commit | Subject | Notes |
|---|---|---|
| `0ac1a01` | ci: add end-to-end --timing-report test on Metal CI | Release checklist item. Also fixed silent failure in inv_chain_pnr step (--sdf without --liberty, masked by tee). Pre-generated .jtir fixture checked in. |
| `87a9e11` | ci: add workflow_dispatch trigger for manual reruns | Needed to trigger CI after repo transfer to gpu-eda org |
| `4022231` | docs(adr): 0013 — cosim peripheral model architecture | ADR documenting CPU-side PeripheralModel trait, GPU-side observe-only vs bidirectional patterns, ring buffers, plural-config convention |
| `6711e9f` | feat(cosim): support multiple UART decoders (#90, ADR 0013) | Schema (effective_uarts), Metal kernel (MAX_UARTS=4 array loop), Rust runtime (multi-buffer alloc, multi-drain). Backward-compatible. |
| `dd5e041` | test(cosim): dual-UART integration test for multi-UART (#90) | Synthesized 496-cell design with two independent TX modules. CI regression gate. |
| `9e5adad` | docs(adr): 0014–0017 — core architecture and cosim execution model | AIG as IR, Boomerang execution model (from GEM paper), X-propagation (promoted), cosim execution model |
| `bef8e81` | docs(adr): fix 9 discrepancies found by code review | Agent-driven review found trait listing gaps, stale claims, metadata field omissions |
| `dfae6ee` | docs(adr): fix 3 remaining ambiguities from re-review | I²C/SPI status, hier range notation, VCD snapshot buffer naming |

## Open follow-ups (priority-ordered)

### 1. Register NVIDIA runner + re-enable CUDA CI (~1h)

Register the runner in the `gpu-eda` org. Then re-enable CUDA CI by removing `if: ${{ false }}` at `.github/workflows/ci.yml` line ~274. The HIP job (line ~363) can also run on NVIDIA hardware initially.

### 2. CUDA/HIP timing-report routing (blocked on #1)

`sim_cuda`/`sim_hip` accept `timing_constraints` but currently use the simple-scan path instead of the timed-batched path that Metal uses. Wire `process_events` and `ReportingCtx` through the CUDA/HIP sim paths. See `docs/release-process.md` pre-release checklist.

### 3. Bump `vendor/eda-infra-rs` submodule (blocked on upstream)

sverilogparse `Cargo.toml` still shows `AGPL-3.0-only` — upstream maintainer acknowledged the typo but hasn't pushed the fix. Check with: `git -C vendor/eda-infra-rs fetch && git show origin/master:sverilogparse/Cargo.toml | grep license`.

### 4. Close issue #90

Multi-UART is implemented and tested. Close the issue with a reference to commits `6711e9f` and `dd5e041`.

## Critical context

- **Repo moved to `gpu-eda` org.** Remote is now `https://github.com/gpu-eda/Jacquard.git`. The `nvlabs` remote still points to the original NVlabs/GEM fork.
- **`--timing-vcd` now accepts `--timing-ir`** (not just `--sdf`/`--liberty`). Fixed in `0ac1a01` — the old guard was too strict.
- **The inv_chain_pnr CI step was silently failing** before this session — `--sdf` without `--liberty` panicked but `tee` masked the exit code. Now uses `--timing-ir` with `set -o pipefail`.

## References

- [`docs/release-process.md`](../release-process.md) — pre-release checklist (3 items remain)
- [`docs/plans/multi-peripheral-cosim.md`](../plans/multi-peripheral-cosim.md) — cosim peripheral plan
- [`docs/adr/README.md`](../adr/README.md) — ADR index (0001–0017)

---

**Resume in a new session with:**
```
/resume_handoff docs/handoffs/release-prep-handoff.md
```
