# Release Process

Lightweight by design. Jacquard is a single-binary Rust project with
`vendor/` submodules; releases are git tags + a CHANGELOG entry. No
crates.io publication, no pre-built binaries (until/unless that
demand surfaces).

## When to release

Cut a release when:

- A user-visible feature or fix lands that you want consumers to be
  able to pin against.
- Schema or CLI changes happened (`--timing-report` JSON, CLI flags)
  and consumers need a stable reference point.
- A meaningful chunk of work in `docs/plans/` is closed (e.g. a Phase
  exits all criteria).

There is no fixed cadence.

## Versioning

[SemVer](https://semver.org/), starting once the first numbered
release ships. Pre-1.0 versions (`0.x.0`) carry the standard SemVer
caveat: minor bumps may include breaking changes; the public
contracts (`--timing-report` schema, IR layout) are documented in their
own ADRs and follow stricter rules.

Stable contracts (additive-only, breaking changes require a major
bump and a deprecation window):

- `--timing-report` JSON schema — `src/timing_report.rs::SCHEMA_VERSION`,
  governed by ADR 0008.
- Timing IR FlatBuffers schema — `crates/timing-ir/schemas/timing_ir.fbs`,
  governed by ADR 0002.

CLI flags, log message formats, and `--timing-summary` text output are
**not** stable parseable contracts; consumers that need to script
against them should use `--timing-report` JSON.

## Steps

For maintainers cutting a release:

1. **Verify CI is green** on `main` for all three GPU backends (Metal,
   CUDA, HIP) plus the unit-test, opensta-to-ir, and lint jobs. If any
   GPU runner is offline, hold the release until it's restored — see
   [`.github/workflows/ci.yml`](https://github.com/gpu-eda/Jacquard/blob/main/.github/workflows/ci.yml). Do not
   ship a binary the CI hasn't built.

2. **Roll the `[Unreleased]` section in [`CHANGELOG.md`](https://github.com/gpu-eda/Jacquard/blob/main/CHANGELOG.md)
   into a numbered version block.** Format follows
   [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Update
   the link references at the bottom of the file. Leave a fresh empty
   `[Unreleased]` section at the top.

3. **Bump the Rust crate version** to match:
   `python3 scripts/bump_version.py <X.Y.Z>`. The three first-party
   Rust crates (`jacquard`, `opensta-to-ir`, `timing-ir`) ship together
   in one tarball and carry a single shared version, so this script sets
   all three at once — never edit their `[package].version` by hand.
   Then `cargo build` to update `Cargo.lock`. (`netlist-graph` versions
   independently — see its own `netlist-graph-v*` tag flow.) The release
   workflow re-runs `bump_version.py --check <tag>` as a verify-guard and
   aborts before publishing if the tag and the crates disagree.

4. **Commit:** `chore: release v<X.Y.Z>` with the standard
   `Co-developed-by` trailer.

5. **Tag:** `git tag -a v<X.Y.Z> -m "v<X.Y.Z>"` then `git push --tags`.

6. **Create a GitHub release** from the tag. Body = the CHANGELOG
   section for that version. No artefacts attached unless someone has
   asked for them.

## Homebrew tap (automated)

The Homebrew formula is **auto-bumped by `release.yml`** — no manual step
(this closes the drift that had left the tap stale at 0.2.3). On every
release tag the `bump-tap` job rewrites `packaging/homebrew/jacquard.rb`'s
`url`/`version`/`sha256` from the just-published tarball + `.sha256` and
pushes `Formula/jacquard.rb` to the tap:

- **final release** → `gpu-eda/homebrew-tap` (`brew install gpu-eda/tap/jacquard`);
- **prerelease (RC)** → `gpu-eda/homebrew-tap-prerelease`
  (`brew install gpu-eda/tap-prerelease/jacquard`), so RCs are
  `brew install`-able for staging without touching the stable channel.

`packaging/homebrew/jacquard.rb` is the **template** — edit it only to change
the formula's *structure* (deps, install, test); its version pin is a
placeholder the job overwrites. Requires the one-time org setup:
`secrets.HOMEBREW_TAP_TOKEN` (a token with `contents: write` on both tap
repos) and the `gpu-eda/homebrew-tap-prerelease` repo.

## Release notes & versioned docs (automated)

Release notes come from the CHANGELOG, and doc links are pinned to the release
— both automatic in `release.yml`:

- **Notes body** = the CHANGELOG section for the tag's version. A prerelease
  (`X.Y.Z-rc.N`) has no dated section, so the extractor falls back to
  `[Unreleased]` — RCs ship the same curated draft you'll ship at promotion.
  So: **write the notes in `[Unreleased]`**.
- **Lead with a user-facing overview.** Before the technical `### Added` /
  `### Changed` sections, open with a short *"What this means for you"* block —
  a few benefit-framed bullets (what a user can now *do*, not just what changed).
  The technical changelog then gives the detail. This becomes the release intro
  and is the first thing a reader sees.
- **Doc links are version-pinned.** The extractor rewrites `` `docs/foo.md` ``
  references into `[docs/foo.md](https://gpu-eda.github.io/Jacquard/<tag>/foo.html)`
  — the mdBook page frozen for *this* release. So keep CHANGELOG doc references
  as repo-relative `` `docs/foo.md` `` (clean in-repo); the workflow does the
  URL rewrite.
- **Versioned docs deploy.** The `docs-version` job publishes the book to
  gh-pages `/(tag)/` on every release tag (`keep_files: true`), while the
  `main` push keeps deploying "latest" to the site root. Version subdirs
  accumulate side by side; the pinned links above resolve to them.
- **The version picker** (`theme/version-picker.js`) reads `versions.json` at
  the site root and lets a reader move between main's docs and the frozen
  releases. **main is the default** and stays at the root, so anyone arriving
  without a version in the URL reads main HEAD; a pinned build is flagged in the
  control, since someone who followed a link out of a release note has no other
  cue that the page is frozen. `versions.json` is regenerated after each deploy
  by `docs/scripts/refresh_doc_versions.sh`, **derived from the directories actually
  published** rather than accumulated — so pruning an old version directory also
  removes it from the picker, and the control can never offer a 404. Release
  candidates are deliberately excluded: their docs stay published (RC notes link
  into them) but they would crowd out the releases people want.

## Staging validation (release candidates)

Optional but recommended before a user-facing release: prove the
*documented install commands* work against a staging artifact before
promoting. There is no "test crates registry", so a **GitHub prerelease**
is the staging tier for the binary channels.

> **Sequence matters — do NOT roll main to the final version before the RC
> validates.** During the RC window `main` **is** the candidate: it stays at
> `X.Y.Z-rc.N` with the changelog still under `[Unreleased]`. Only the
> **Promote** step (below) rolls `[Unreleased] → [X.Y.Z]` and bumps to the
> final `X.Y.Z`. Rolling the release onto `main` first leaves `main`
> advertising a version that has no release — the version pin and dated
> changelog claim `X.Y.Z` is shipped while only the RC tag exists, which also
> breaks `cargo binstall --git` (it reads `main`'s version and fetches a
> non-existent `vX.Y.Z` tarball). Each new RC is a `main` commit bumped to the
> next `-rc.N`; `main == the latest RC tag` throughout.

1. **Cut an RC.** On `main` at `X.Y.Z-rc.N` (changelog under `[Unreleased]`),
   `python3 scripts/bump_version.py <X.Y.Z>-rc.<N>`, commit, tag
   `v<X.Y.Z>-rc.<N>`, push the tag. `release.yml` detects the SemVer
   pre-release suffix and publishes a GitHub **prerelease** (never shown as
   "Latest") with the Metal tarball attached, and `bump-tap` pushes the
   formula to `gpu-eda/homebrew-tap-prerelease`.

2. **Validate.** Dispatch the **Validate install (staging)** workflow
   (`validate-install.yml`) with that tag. It runs the real install
   commands against the prerelease asset on macOS:
   - `cargo binstall` (asset-fetch via the `[package.metadata.binstall]`
     override, compile fallback disabled so a missing asset fails hard);
   - `brew install` of an RC formula (the source-of-truth formula repointed
     at the prerelease tarball + its `.sha256`, installed from a throwaway
     local tap).

3. **Promote.** A green run means the channels work. Bump to the final
   `<X.Y.Z>` (drop the `-rc.<N>`), commit, tag `v<X.Y.Z>`, push — the same
   flow as a normal release below.

The `netlist-graph` (PyPI) channel validates separately via its own
`workflow_dispatch` → TestPyPI path (see `publish-netlist-graph.yml`); it
versions independently of the Rust crates.

## What does NOT need to change at release time

- Submodule pins (unless deliberately bumping a vendored dep).
- The `vendor/opensta/` submodule pin is the version named in
  `crates/opensta-to-ir::MIN_TESTED_OPENSTA_VERSION`. If you bump the
  submodule, also bump the constant and the version-probe test — see
  WS-RH.1 in [`docs/plans/post-phase-0-roadmap.md`](plans/post-phase-0-roadmap.md).
- `LICENSE` (unless re-licensing).

## Pre-release checklist (one-time, before the first numbered release)

These items are tracked in [`docs/plans/post-phase-0-roadmap.md`](plans/post-phase-0-roadmap.md)
§ Release hardening; this section is the visible punch-list:

- [x] Phase 1 (ADR 0008 required outputs) closed.
- [x] WS-RH.1 (OpenSTA detection + version check) shipped.
- [x] Metal CI on `macos-runner-1` green (re-enabled in commit `12e98df`,
      2026-05-12).
- [ ] **CUDA CI** on `nvidia-runner-1` green on main. Currently
      disabled in `.github/workflows/ci.yml` (`if: ${{ false }}`,
      ~line 268). Re-enable when hardware lands.
- [ ] **HIP CI** on the AMD runner green on main. Currently disabled
      in `.github/workflows/ci.yml` (`if: ${{ false }}`, ~line 357).
      Re-enable when the AMD runner is online.
- [ ] **Prebuilt CUDA/HIP binaries** (ADR 0018 Phase 4), when produced, must
      build with `JACQUARD_CUDA_ARCH=all-major` so the kernel ships portable
      SASS for every major arch (`sm_50`…`sm_120` on CUDA ≥ 12.8, Blackwell
      included) plus PTX for the newest — see the README § CUDA target
      architecture. Local dev uses `JACQUARD_CUDA_ARCH=native` instead. The
      `nvidia1.local` Blackwell box (`sm_120`, CUDA 12.8) is a candidate
      self-hosted CUDA release runner.
- [x] Vendored-dep license posture confirmed
      ([gzz2000/eda-infra-rs#2](https://github.com/gzz2000/eda-infra-rs/issues/2#issuecomment-4363789319)
      — sverilogparse AGPL declaration acknowledged as a typo; workspace
      Apache-2.0 governs).
- [x] `Cargo.toml::license = "Apache-2.0"` set.
- [x] `NOTICE` file enumerating vendored deps + their licenses.
- [ ] **Bump `vendor/eda-infra-rs` submodule** once upstream pushes the
      sverilogparse `Cargo.toml` correction; remove the corresponding
      footnote in `NOTICE`. Maintainer acknowledged the typo on
      2026-05-02 but hasn't pushed the fix as of 2026-05-13. Verify
      with `git -C vendor/eda-infra-rs fetch && git log origin/master --oneline`.
- [x] **CUDA / HIP runtime violation routing** through
      `process_events` — done (commit `24723b5`, issue #104). `sim_cuda` /
      `sim_hip` now dispatch the timed-batched path and drain violation
      events, so `--timing-report` / `--timing-summary` / `--timed` are no
      longer Metal-only.
- [x] Bounded violations array (`--timing-report-max-violations`,
      default 100k).
- [x] End-to-end `--timing-report` test on Metal CI. The
      `inv_chain_pnr` sim step uses `--timing-ir` (pre-generated
      `.jtir` checked in) + `--timing-report` + `--timing-summary`;
      a follow-up step validates the JSON shape (top-level keys,
      semver schema version, metadata, stats, arrays).
- [x] GF180MCU enablement (Phases 0–6) shipped. See
      `docs/plans/gf180mcu-enablement.md`. Phase 7 (wafer.space
      test-run-1 design integration) deferred pending design
      availability; not release-blocking.

## License posture

Project license is Apache-2.0 (`LICENSE`). Vendored-dep posture is
enumerated in `NOTICE`. Summary:

- `vendor/eda-infra-rs/` — Apache-2.0 (workspace). The `sverilogparse`
  sub-crate's stale `AGPL-3.0-only` declaration in `Cargo.toml` is a
  typo per upstream maintainer
  ([gzz2000/eda-infra-rs#2](https://github.com/gzz2000/eda-infra-rs/issues/2#issuecomment-4363789319));
  governed by the workspace LICENSE. Submodule pin will be bumped when
  upstream pushes the correction.
- `vendor/sky130_fd_sc_hd/` — Apache-2.0.
- `vendor/opensta/` — GPL-3 (subprocess only per ADR 0001 + ADR 0006
  § Amendment; never linked, never bundled).

## Cross-references

- [`CHANGELOG.md`](https://github.com/gpu-eda/Jacquard/blob/main/CHANGELOG.md) — release log.
- [`docs/adr/0008-structured-timing-output.md`](adr/0008-structured-timing-output.md)
  — `--timing-report` stability contract.
- [`docs/adr/0002-timing-ir.md`](adr/0002-timing-ir.md) — IR schema
  versioning.
- [`docs/adr/0006-sdf-preprocessing-model.md`](adr/0006-sdf-preprocessing-model.md)
  — OpenSTA bundling rules.
- [`docs/project-scope.md`](project-scope.md) — license posture
  contract.
