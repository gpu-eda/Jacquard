# Handoff — distribution & release hardening

**Created:** 2026-06-23 · **Branch:** `feat/user-acceptance-and-dist-docs`
(in-flight) off `main`.

## Goal & now

Bring the ADR 0018 distribution layer to "actually works": ship a usable Metal
binary, gate releases on a user-level install+smoke, and document the channel
reality. **Now:** finish + merge the in-flight branch (PR pending), then **cut
v0.2.1** (the first release whose `release.yml` produces an attached, working
binary).

## State of the release

- **v0.1.0** tagged at `7fed695` (2026-06-04 snapshot) + GitHub release — **no
  binary asset** (the release workflow failed; see below).
- **v0.2.0** tagged at `a9308d4` (2026-06-23) + GitHub release — **no binary
  asset** either.
- Both failed to attach a binary because `release.yml`'s publish step ran
  `gh release create` and **`gh` is not on PATH on `macos-runner-1`**. The
  binary *was* built + smoke-tested; only publish died.
- **Fixed in #131 (merged):** publish now uses `softprops/action-gh-release`
  (token API, no `gh`); the `sim` Metal kernel is now embedded
  (`include_bytes!`, was loading a build-tree path → broken on relocated
  binaries); `crates/timing-ir/Cargo.toml` repo URL → `gpu-eda`.
- **v0.2.1 not yet cut.** Tag-triggered workflows use the workflow file *at the
  tag*, so v0.1.0/v0.2.0 can't be cleanly re-run; v0.2.1 is the first tag with
  the fixed `release.yml` + relocatable `sim` → first real binary. Plan:
  roll a `[0.2.1]` "distribution fixes" CHANGELOG entry, bump `Cargo.toml`
  0.2.0→0.2.1, commit, tag after main CI green, confirm the asset attaches.

## In-flight branch `feat/user-acceptance-and-dist-docs` (needs PR + merge)

- `scripts/ci/user_acceptance_smoke.sh` — shared post-install verify
  (`--version`, **sim**, **cosim** apb_trace per `docs/installation.md` §
  Verify). Tested locally against a relocated binary. The `sim` coverage is the
  gap that let v0.2.0 ship broken.
- `.github/workflows/release.yml` — its relocated-smoke step now calls that
  script (gates publish).
- `.github/workflows/user-acceptance.yml` — new standalone `workflow_dispatch`:
  build → install to a clean dir → run the smoke script.
- `.github/workflows/publish-netlist-graph.yml` — added a `publish-testpypi`
  job (`workflow_dispatch`, environment `testpypi`, `repository-url`
  test.pypi.org) so a dispatch validates the OIDC wiring before the real tag.
- `docs/installation.md` — `cargo binstall jacquard` → `cargo binstall --git …`
  (jacquard isn't on crates.io; see Findings).
- `docs/plans/distribution.md` — added **Phase 7** (eda-infra-rs upstreaming).

## Channel state (what actually works)

- **Prebuilt tarball + `cargo binstall --git`**: live from **v0.2.1** (binstall
  metadata in `Cargo.toml` is correct; `--git` form required, not crates.io).
- **Homebrew**: tap `gpu-eda/homebrew-tap` exists but is **empty** (README
  only). Source-of-truth formula `packaging/homebrew/jacquard.rb` points at
  **v0.1.0** with a **placeholder sha256**. To go live: after v0.2.1, bump the
  formula `url`/`version`/`sha256` to the v0.2.1 tarball and push it to the tap
  as `Formula/jacquard.rb`.
- **PyPI `netlist-graph`** (version `0.1.0`): trusted publishers configured by
  the maintainer for env `pypi` (real) and `testpypi`. Not published yet.
  Validate via dispatch → TestPyPI, then tag `netlist-graph-v0.1.0` for real
  PyPI.

## Staging-validation design (agreed direction, not built)

Docs keep the standard install commands; a **pre-release** test runs *those
commands against staging*: Python via **Test PyPI** (`--index-url`); the two
GitHub-release channels (binstall + brew) via a **prerelease GitHub release**
(RC tag) — there is no "test crates registry", a prerelease *is* the staging
tier — with `cargo binstall --git --version <rc>` and
`brew install --formula packaging/homebrew/jacquard.rb`. A green run promotes
to the real release / PyPI / tap. **Build this as the next layer after v0.2.1.**

## Findings

- **No crates.io** for `jacquard`: (1) version-less path deps `opensta-to-ir` /
  `timing-ir` (timing-ir is `publish = false`) — but those are *ours* to fix;
  (2) the real blocker is the **vendored fork** `vendor/eda-infra-rs`
  (`ChipFlow/eda-infra-rs`) — `version+path` deps resolve to the fork's patched
  code, and the fork has diverged from the declared versions. Hence
  `cargo binstall` needs `--git`.
- **eda-infra-rs upstreaming audit** — full table + tasks now in
  `docs/plans/distribution.md` Phase 7. TL;DR: fork is 13 ahead / 1 behind;
  ANSI-ports = open PR gzz2000#3, Metal = **draft** gzz2000#1, **HIP backend
  (~9 commits) + unary-NOT not PR'd**, and we haven't pulled the upstream
  license fix `026070c`.

## Open follow-ups (priority order)

1. PR + merge the in-flight branch, then **cut v0.2.1** (confirm the Metal
   tarball asset attaches via the fixed workflow).
2. **Homebrew formula** bump → push to the tap (after v0.2.1).
3. **netlist-graph** → dispatch to TestPyPI → verify → tag `netlist-graph-v0.1.0`.
4. **Staging install-validation** (RC-prerelease pipeline) per the design above.
5. **eda-infra-rs upstreaming** (plan Phase 7): pull `026070c` + re-pin first.
6. **Stale upstream PRs** #94/#57/#53/#17 are not safely mergeable (2 conflict,
   2 are ~110 commits behind with stale CI) — triage/rebase or close, don't
   blind-merge onto the freshly-released main.

## References

- ADR 0018 (`docs/adr/0018-distribution-and-installation.md`), plan
  (`docs/plans/distribution.md`), `packaging/README.md`.
- Merged: #131 (release/sim/url fixes). Releases v0.1.0 / v0.2.0 (binary-less).
