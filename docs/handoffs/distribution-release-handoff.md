# Handoff — distribution & release hardening

**Created/updated:** 2026-06-24 · main @ `87f0dcd`, CI green.

## Goal & now

Bring the ADR 0018 distribution layer to "actually works". **The core is
done:** v0.2.1 ships a working Metal binary, three install channels are
live, and the Rust crate versions are unified behind a verify-guard.
**Now:** the remaining work is the *staging-validation* pipeline and two
upstream-hygiene items (see Open follow-ups). No release is in flight.

## What shipped (2026-06-24)

- **v0.2.1 released** at `056eb2b` (tag `v0.2.1`) — the **first release with
  an attached, working binary asset**: `jacquard-0.2.1-macos-arm64-metal.tar.gz`
  (+ `.sha256`). The fixed `release.yml` (softprops/action-gh-release, embedded
  metallib) plus the user-acceptance smoke gate verified the relocated binary
  *before* publish. v0.1.0 / v0.2.0 remain binary-less (historical; their
  tag-pinned workflows can't be cleanly re-run).
- **Homebrew channel LIVE.** `gpu-eda/homebrew-tap` now has
  `Formula/jacquard.rb` (v0.2.1, real sha256). Source-of-truth
  `packaging/homebrew/jacquard.rb` bumped to match. Install-tested
  end-to-end (`brew install gpu-eda/homebrew-tap/jacquard` + `brew test`).
- **netlist-graph 0.1.0 on real PyPI.** `pip install netlist-graph` works.
  Validated via TestPyPI dispatch first, then tag `netlist-graph-v0.1.0`.
- **Cargo.lock now tracked** (was gitignored) — reproducible release builds.
- **Rust crate versions unified** (PR #134, merged `87f0dcd`).
  `scripts/bump_version.py` is the single source of truth for the three
  first-party crates (`jacquard` + `opensta-to-ir` + `timing-ir`, the
  helpers moved `0.1.0`→`0.2.1`); `release.yml` runs `--check <tag>` as a
  verify-guard and `ci.yml` Lint runs `--check` on every PR.

## Channel state (what actually works)

- **Prebuilt tarball + `cargo binstall --git`**: live from v0.2.1.
- **Homebrew**: live — `brew tap gpu-eda/homebrew-tap && brew install gpu-eda/homebrew-tap/jacquard`.
- **PyPI `netlist-graph` 0.1.0**: live — `pip install netlist-graph`.

## Findings / learnings

- **PyPI trusted-publisher repo name is case-sensitive.** Both TestPyPI and
  PyPI pending publishers must use `Jacquard` (capital J) to match the GitHub
  OIDC `repository` claim `gpu-eda/Jacquard`. A lowercase `jacquard` config
  fails with `invalid-publisher: valid token, but no corresponding publisher`.
  This bit the first TestPyPI dispatch.
- **No crates.io for `jacquard`**: the vendored fork `vendor/eda-infra-rs`
  (`version+path` deps resolved to patched, diverged code) blocks publishing;
  hence `cargo binstall --git`. Full upstreaming audit in
  `docs/plans/distribution.md` Phase 7.
- **Rust crates are NOT a Cargo workspace** — they build into separate
  `crates/*/target/` dirs that ~7 CI/release/script steps depend on. That's
  why version unification uses a lock-step script, not `[workspace.package]`
  inheritance (the latter would relocate build output and rewrite all 7).

## Staging-validation design (agreed direction, NOT built — this is follow-up #1)

Docs keep the standard install commands; a **pre-release** test runs *those
commands against staging*: Python via **Test PyPI** (`--index-url`); the two
GitHub-release channels (binstall + brew) via a **prerelease GitHub release**
(RC tag) — there is no "test crates registry", a prerelease *is* the staging
tier — with `cargo binstall --git --version <rc>` and
`brew install --formula packaging/homebrew/jacquard.rb` (note: modern brew
rejects a loose formula file; use a tap or `brew install <tap>/jacquard`).
A green run promotes to the real release / PyPI / tap.

## Open follow-ups (priority order)

1. **Staging install-validation** (RC-prerelease pipeline) per the design above.
2. **eda-infra-rs upstreaming** (plan Phase 7): pull upstream license fix
   `026070c` + re-pin first; then the ANSI-ports / Metal / HIP / unary-NOT
   PR work. TL;DR: fork is 13 ahead / 1 behind.
3. **Stale upstream PRs** #94/#57/#53/#17 — not safely mergeable (2 conflict,
   2 are ~110 commits behind with stale CI). Triage/rebase or close; don't
   blind-merge onto the freshly-released main.

## References

- ADR 0018 (`docs/adr/0018-distribution-and-installation.md`), plan
  (`docs/plans/distribution.md` incl. Phase 7), `packaging/README.md`,
  `docs/release-process.md` (now documents `scripts/bump_version.py`).
- Releases: v0.2.1 (first working binary). PRs: #131 (release/sim/url fixes),
  #133 (smoke gate + TestPyPI job + docs), #134 (version unification).
