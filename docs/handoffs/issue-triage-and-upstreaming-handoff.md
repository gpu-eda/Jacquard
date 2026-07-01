# Handoff — upstream hygiene (eda-infra-rs + stale PRs)

**Created/updated:** 2026-07-01 · branch `docs/macos-split-handoff`.

## Goal & next-up

The **issue triage is done** (2026-07-01) — see "Triage outcome" below. What
remains from the original arc are the two **upstream-hygiene follow-ups**:
eda-infra-rs upstreaming (Phase 7) and the stale upstream PRs. Pick either up
cold from the "Open follow-ups" section.

## Triage outcome (resolved — 2026-07-01)

Triaged all open issues against `f6d0e23`. **No close candidates remained** —
the strong-RESOLVED set (#104, #105, #100, #7, #130) had already been closed on
2026-06-26/30. Every still-open issue is genuinely open. Three got scoping
comments recording their partial-progress state (the permanent home for the
triage conclusions):

- **#6** multi-corner SDF — timing-IR path has `--timing-corner` (single-select);
  SDF path still single-corner (`--sdf-corner`, no `all`). Comment posted.
- **#9** WNS/TNS/WHS/THS — WNS+WHS + violation counts ship; **TNS/THS** (total-slack
  sums) not yet computed. Comment posted.
- **#10** multi-clock — `MultiClockScheduler` shipped for cosim *execution*
  (`src/sim/cosim/mod.rs:1309`); per-domain *timing analysis* + CDC-path flagging
  not built. Pairs with #87. Comment posted.

Everything else stays open with no change: #92 (CDC jitter, well-scoped
checklist), #103/#119 (priority:high), #127 (non-determinism), #87/#106/#107
(coverage/roadmap), #142/#143 (new bugs), and the forward-looking timing
roadmap #8/#11–#16.

Also folded this session: the org-rename currency pass had missed 10 live
`ChipFlow/Jacquard` hyperlinks across 4 doc files — repointed to
`gpu-eda/Jacquard` (commit `f6d0e23`). The handoff's flagged ADR-0011
`#80`→`#103` "cross-ref bug" was a non-issue: the amendment already cites #103
correctly; the `#80` references legitimately track the original single-SRAM
feature (closed).

## Open follow-ups (priority order)

### 1. eda-infra-rs upstreaming (plan `docs/plans/distribution.md` Phase 7)

Pull upstream license fix `026070c` + re-pin the submodule first; then the
ANSI-ports / Metal / HIP / unary-NOT PR work. Path to a crates.io publish.

### 2. Stale upstream PRs #94 / #57 / #53 / #17

Not safely mergeable as-is; triage/rebase or close each.

## Critical context (carry forward)

- PyPI trusted-publisher repo name is case-sensitive (`gpu-eda/Jacquard`).
- cargo-binstall `--git` can't pin a tag; pass `GITHUB_TOKEN` or it 403s.
- Modern brew rejects loose formula files; use a local tap + fully-qualified install.
- Immutable-releases repo: publish prereleases via a draft.
- Prebuilt binary needs Homebrew LLVM at runtime (libc++/libomp via mt-kahypar).
- Rust crates are NOT a Cargo workspace (separate `crates/*/target/`).
- CI: self-hosted `tesla4-runner` (CUDA/HIP) + `macos-runner-1` (Metal) are single
  shared runners — can queue/stall; docs/dist-only PRs are safe `--admin` merges
  when GPU-relevant checks are green.

## References

- `docs/plans/distribution.md` (Phase 7), `docs/release-process.md`,
  `docs/installation.md`.
- ADRs under `docs/adr/` (all current as of the `e56e831` currency pass).
- PRs this arc: #133–#140. Releases v0.2.1 → v0.2.3.

---

**Resume in a new session with:**
```
/resume_handoff docs/handoffs/issue-triage-and-upstreaming-handoff.md
```
