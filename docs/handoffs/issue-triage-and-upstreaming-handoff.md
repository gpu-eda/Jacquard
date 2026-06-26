# Handoff — issue triage & upstream hygiene

**Created/updated:** 2026-06-26 · main @ `e56e831`, CI green.

## Goal & now

The distribution-and-release arc is **done** (folded into ADR 0018's amendment,
`docs/release-process.md`, and the install docs — see "What shipped"). The
active task is now a **triage of the 22 open GitHub issues against current
state** — several are stale/resolved by what shipped this session. After that,
two standing upstream-hygiene follow-ups remain.

## Active task: issue triage (just started, interrupted before agents ran)

Review all **22 open issues** against the current code; decide close / keep /
update-with-comment for each. **Do NOT auto-close** — produce a triage table
and let Rob decide. The intended approach (interrupted): fan out ~4 parallel
`scout` agents, each `gh issue view`s a group of issues, verifies against
`src/`/`csrc/`/`CHANGELOG.md`/`docs/adr/`, and reports RESOLVED / PARTIAL /
OPEN with file:line evidence + a recommended action.

**Strong RESOLVED candidates (verify, then propose closing):**
- **#104** "sim: CUDA/HIP drop timing constraints — `--timing-report`/
  `--timing-summary`/`--timed` are Metal-only" → **wired** (commit `24723b5`;
  confirmed in the ADR 0008 amendment). Almost certainly closeable.
- **#105** "cosim is Metal-only — make backend-portable (CUDA/HIP)" → CUDA/HIP
  cosim backends now exist (`src/sim/cosim/cuda.rs`, `hip.rs`; ADR 0013/0017
  amendments). Check whether the CPU-fallback half is also done before closing.

**Likely PARTIAL / needs nuance:**
- **#6** multi-corner SDF (min+max) — multi-corner *timing IR* shipped via
  `--timing-corner`, but `--sdf-corner` is still one-corner-at-a-time.
- **#9** WNS/TNS/WHS/THS summary — `--timing-summary` ships; confirm the exact
  metrics match.
- **#10** multi-clock domain — `MultiClockScheduler` shipped for cosim; confirm
  scope vs the issue's ask.
- **#7** clock skew from SDF — per-DFF clock-arrival folding (Pillar B) shipped;
  confirm this is what #7 wanted.
- **#92** complete ADR 0012 CDC jitter (deferred scope) — still partial (ADR
  0012 is `Accepted (partial)`).
- **#103** multi-SRAM `sram_init` — still OPEN (ADR 0011 amendment confirms
  single-SRAM only). ⚠ **Cross-ref bug:** ADR 0011's new amendment cites
  "issue #80" for this — the actual open issue is **#103**; verify/fix that
  citation.

**Likely still OPEN (forward-looking timing roadmap, 2026-02-16 batch):**
- #11 OCV derating, #12 crosstalk/SI, #13 IR-drop, #14 PBA, #15 SSTA, #16 ECO.
- #127 non-deterministic output state-slot (mt-kahypar) — relates to the
  HashMap-ordering heisenbug (see user memory `feedback_hashmap_ordering`).
- #100 netlist-graph drivers walk clk/set/reset; #106/#107 X-barrier
  assertions; #119/#130 GF180MCU cosim SRAM-race / 7T-hardcode; #87 multi-clock
  test coverage.

`gh issue list --state open --limit 100` for the live list.

## What shipped (context — this arc is resolved)

- **v0.2.3 released** (docs release; first working prebuilt channels were
  v0.2.2). Three live install channels: `cargo binstall --git` (needs
  `brew install llvm`), Homebrew tap `gpu-eda/homebrew-tap` (auto-LLVM),
  PyPI `netlist-graph 0.1.0`.
- **Staging install-validation** (`validate-install.yml`) — the gate that
  caught v0.2.1's broken channels; RC → validate → promote flow.
- **Unified crate versions** (`scripts/bump_version.py` + verify-guard);
  **Cargo.lock tracked**; prerelease draft-publish fix for immutable releases.
- **Docs currency pass**: README getting-started polish, latch-limitation
  clarified across the book, stale `ChipFlow`→`gpu-eda` links fixed.
- **ADR amend-in-place convention** (`docs/adr/README.md`): refinement → dated
  in-place Amendment (original preserved); reversal → supersede. Applied as a
  **currency pass amending 14 ADRs** (`e56e831`) — incl. 0018 Proposed→Accepted,
  0008 Approved→Accepted, 0013/0017 cosim-Metal-only corrections.

## Findings / learnings (carry forward)

- PyPI trusted-publisher repo name is case-sensitive (`gpu-eda/Jacquard`).
- cargo-binstall `--git` can't pin a tag; pass `GITHUB_TOKEN` or it 403s.
- Modern brew rejects loose formula files; use a local tap + fully-qualified
  install.
- Immutable-releases repo: publish prereleases via a draft.
- Prebuilt binary needs Homebrew LLVM at runtime (libc++/libomp via mt-kahypar).
- Rust crates are NOT a Cargo workspace (separate `crates/*/target/`).
- CI gotchas: the self-hosted `tesla4-runner` (CUDA/HIP) and `macos-runner-1`
  (Metal) are single shared runners — CI can queue/stall on them or on a
  spending-limit; docs/dist-only PRs are safe `--admin` merges when the
  GPU-relevant checks are green.

## Open follow-ups (priority order)

1. **Finish the issue triage** (active task above) — propose close/keep/update
   per issue; fix the ADR 0011 `#80`→`#103` cross-ref.
2. **eda-infra-rs upstreaming** (plan `docs/plans/distribution.md` Phase 7):
   pull upstream license fix `026070c` + re-pin the submodule first; then the
   ANSI-ports / Metal / HIP / unary-NOT PR work. Path to a crates.io publish.
3. **Stale upstream PRs** #94/#57/#53/#17 — not safely mergeable; triage/rebase
   or close.

## References

- ADRs (`docs/adr/`, all current as of the `e56e831` currency pass),
  `docs/release-process.md`, `docs/installation.md`, `docs/plans/distribution.md`.
- PRs this arc: #133–#140. Releases v0.2.1 → v0.2.3.
