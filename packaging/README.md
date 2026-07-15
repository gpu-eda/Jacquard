# Packaging & distribution

How Jacquard's artifacts reach users. Design rationale: [ADR 0018](../docs/adr/0018-distribution-and-installation.md);
phasing: [`docs/plans/distribution.md`](../docs/plans/distribution.md).

| Artifact | Channel | Built by |
|----------|---------|----------|
| `jacquard` + `opensta-to-ir` (macOS/Metal) | GitHub Release + `cargo binstall` + Homebrew | `.github/workflows/release.yml` (on `v*`) |
| `netlist-graph` (Python) | PyPI | `.github/workflows/publish-netlist-graph.yml` (on `netlist-graph-v*`) |

## Rust binaries — GitHub Releases

`release.yml` builds on a `v<X.Y.Z>` tag, packages the binaries at the
archive root (matching the `cargo-binstall` `bin-dir` in `Cargo.toml`),
smoke-tests the relocated binary, and creates a **draft** release. Review
the assets, then publish.

`cargo binstall jacquard-sim` then resolves the macOS/Metal asset (the
package is `jacquard-sim`; the binary it installs is `jacquard`). Linux is
not binstall-able — two GPU backends per target triple; use the tarball
or a container.

## Homebrew tap (macOS/Metal)

The tap repo — **[gpu-eda/homebrew-tap](https://github.com/gpu-eda/homebrew-tap)** —
is provisioned (scaffolded with `brew tap-new`: `brew test-bot` CI +
`pr-pull` bottle publishing). It has no formula yet; the formula lands
with the first release.

The formula lives at [`homebrew/jacquard.rb`](homebrew/jacquard.rb) here
as the source of truth. To add/update it in the tap (the idiomatic
Homebrew flow):

1. After tagging `v<X.Y.Z>` and publishing the release, take the
   `.sha256` the release workflow emitted.
2. Open a PR to the tap adding/updating `Formula/jacquard.rb` (copy of
   `packaging/homebrew/jacquard.rb` with the real `url` / `version` /
   `sha256`). The tap's CI installs + tests it on the PR.
3. Merge (optionally label `pr-pull` to publish bottles).

`brew bump-formula-pr` automates steps 1–2 for subsequent releases; a
release-CI step could open the PR automatically (plan Phase 2 stretch).

Users then:

```sh
brew install gpu-eda/tap/jacquard      # jacquard + opensta-to-ir
```

## netlist-graph — PyPI

`publish-netlist-graph.yml` builds the wheel and publishes via PyPI
**trusted publishing** (OIDC, no stored token) on a `netlist-graph-v*`
tag. One-time setup on PyPI:

1. Create/own the **`netlist-graph`** project on PyPI.
2. Add a trusted publisher: owner `gpu-eda`, repository `Jacquard`,
   workflow `publish-netlist-graph.yml`, environment `pypi`.
3. Create a `pypi` environment in the repo settings (optionally with
   required reviewers).

Users then:

```sh
uvx netlist-graph search design.gv psel    # or: pip install netlist-graph
```

## Dry-runs

Both workflows accept `workflow_dispatch` for a dry-run: they build +
upload the artifact but do not publish/release. Use these to validate the
pipeline before tagging.
