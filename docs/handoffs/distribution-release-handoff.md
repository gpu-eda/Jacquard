# Handoff — distribution & release hardening

**Created/updated:** 2026-06-25 · main @ `c7af356`, CI green.

## Goal & now

The ADR 0018 distribution layer is **done**: **v0.2.2 is the first release
whose prebuilt macOS channels work on a clean machine**, validated end-to-end
(`validate-install` green on both binstall + brew against the real v0.2.2 —
the same gate is *red* on v0.2.1). Homebrew tap live, docs current. **Now:**
the only remaining work is upstream hygiene — see Open follow-ups. No release
in flight.

## What shipped (this arc, 2026-06-23 → 06-25)

- **v0.2.2 released** (`34d83ef`, tag `v0.2.2`). Tarball ships `jacquard` +
  `timing_analysis` + `opensta-to-ir`; sha `38e3c895…`. Validated green on
  both channels on the real release.
- **Staging install-validation pipeline** (`validate-install.yml`,
  `workflow_dispatch <tag>`): runs the documented `cargo binstall` + `brew
  install` against a published (pre)release on clean `macos-latest`. RC tags
  (`vX.Y.Z-rc.N`) publish as GitHub *prereleases*; green validation gates
  promotion. Docs: `release-process.md § Staging validation`.
- **Two real bugs the pipeline caught and fixed** (v0.2.1's prebuilt channels
  were broken on any Mac without Homebrew LLVM): (1) tarball omitted
  `timing_analysis` (a non-optional `jacquard` bin) → `cargo binstall` failed;
  now shipped. (2) binary links Homebrew LLVM `libc++`/`libomp` → formula now
  `depends_on "llvm"`; binstall/raw-tarball users `brew install llvm`
  (documented).
- **Prerelease publish fix** (PR #137): immutable-releases repo rejects asset
  upload to a published prerelease → prereleases publish via a draft.
- **Unified Rust crate versions** (PR #134): `scripts/bump_version.py` single
  source of truth; `release.yml`/`ci.yml` run `--check` as a verify-guard.
- **Homebrew tap live on v0.2.2** (`gpu-eda/homebrew-tap` `a14636f`).
- **netlist-graph 0.1.0 on PyPI**; **Cargo.lock tracked**.
- **README/docs polish** (PR #139): inline brew one-liner; timing-status table
  moved into `docs/timing-simulation.md`; stale `ChipFlow`/`chipflow.github.io`
  links → `gpu-eda`; corrected stale capability claims (CUDA/HIP `sim` now
  route violations; multi-corner shipped; `sim` vs `cosim` limitations).

## Channel state (what actually works)

- **`cargo binstall --git`**: works v0.2.2. **Requires `brew install llvm`** at
  runtime (not auto-installed for this channel).
- **Homebrew**: `brew install gpu-eda/homebrew-tap/jacquard` — live, v0.2.2,
  LLVM auto-installed via `depends_on`.
- **Raw tarball**: works v0.2.2; needs `brew install llvm`.
- **PyPI `netlist-graph` 0.1.0**: `pip install netlist-graph`.

## Findings / learnings (carry forward)

- **PyPI trusted-publisher repo name is case-sensitive** — `gpu-eda/Jacquard`
  (capital J), else `invalid-publisher`. (Memory: `project_pypi_trusted_publisher_case`.)
- **cargo-binstall `--git` can't pin a tag** — validate RC tarballs via
  checkout-at-tag + `--manifest-path`. binstall queries the GitHub API; pass
  `GITHUB_TOKEN` or it 403s on the 60/hr anonymous limit under load.
- **Modern brew rejects loose formula files** + needs tap trust — use a local
  tap + fully-qualified install (`<org>/<tap>/<formula>` needs no `brew trust`).
- **Immutable-releases repo** rejects asset upload to a published prerelease —
  publish prereleases via a draft.
- **The prebuilt binary needs Homebrew LLVM at runtime** (`libc++`/`libomp` via
  mt-kahypar's OpenMP). Not static-linked — deliberate call for this audience.
- **Rust crates are NOT a Cargo workspace** (separate `crates/*/target/` dirs
  ~7 CI steps depend on) — hence the bump-script versioning.

## Open follow-ups (priority order)

1. **eda-infra-rs upstreaming** (plan `docs/plans/distribution.md` Phase 7):
   pull the upstream license fix `026070c` + re-pin the submodule first; then
   the ANSI-ports / Metal / HIP / unary-NOT PR work. Fork is ~13 ahead / 1
   behind. This is the path toward a real crates.io publish.
2. **Stale upstream PRs** #94/#57/#53/#17 — not safely mergeable (2 conflict,
   2 are ~110 commits behind with stale CI). Triage/rebase or close; don't
   blind-merge onto the freshly-released main.

## References

- ADR 0018 (`docs/adr/0018-distribution-and-installation.md`),
  `docs/plans/distribution.md` (Phase 7), `packaging/README.md`,
  `docs/release-process.md` (bump script + staging validation).
- PRs this arc: #133, #134 (version unify), #135 (staging pipeline),
  #136 (channel fixes), #137 (prerelease draft-publish), #138 (binstall token),
  #139 (README/docs). Releases: v0.2.2 (first working prebuilt channels).
