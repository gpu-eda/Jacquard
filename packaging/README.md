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

`cargo binstall jacquard` then resolves the macOS/Metal asset (Linux is
not binstall-able — two GPU backends per target triple; use the tarball
or a container).

## Homebrew tap (macOS/Metal)

The formula lives at [`homebrew/jacquard.rb`](homebrew/jacquard.rb) as
the source of truth. To stand up the tap:

1. Create the repo **`gpu-eda/homebrew-tap`**.
2. Copy `packaging/homebrew/jacquard.rb` → `Formula/jacquard.rb` in it.
3. Per release, update the formula's `version` + `sha256` to the released
   tarball (the release emits a `.sha256`; or use `brew bump-formula-pr`).

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
