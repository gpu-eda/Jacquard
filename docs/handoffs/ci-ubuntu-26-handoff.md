# Handoff — CI on Ubuntu 26.04 (mt-kahypar no longer compiles on `ubuntu-latest`)

**Created:** 2026-11-12
**Working tree:** clean
**Branch:** `ci/pin-ubuntu-24.04` (off `main` at `90d24791`; worktree `~/Code/jacquard-ci-ubuntu-pin`)

## Goal & next-up

**Goal of this session:** resolve the architecture-docs migration handoff (PR
#237). Its Documentation check failed for an unrelated reason: GitHub moved
`ubuntu-latest` to Ubuntu 26.04, and mt-kahypar's C++ no longer compiles there.

**Next session should pick up:** on this branch, pin every `runs-on:
ubuntu-latest` to `ubuntu-24.04`, delete this handoff in the same PR, and land
it. Then rebase #237 onto main so its Documentation check reruns on 24.04.

**Verification command:**
```sh
cd ~/Code/jacquard-ci-ubuntu-pin
git grep -n "ubuntu-latest" -- .github
# Expect after the fix: no matches (the cpu matrix entry in build.yml included)
actionlint .github/workflows/*.yml
gh pr checks 237   # after #237 is rebased: Documentation passes
```

## Done this session

| Commit / PR | Subject | Notes |
|---|---|---|
| #236 `d50fecf9` | ci(docs): cache cargo's install records | Merged. Fixed the mdbook-d2 "binary already exists" cache failure |
| #237 `f4335973` | docs: resolve architecture-docs migration handoff | Open. Blocked only by the Documentation failure below |
| #235 review | changes requested on the VCD compact-value PR | Waiting on the external contributor |

## Open follow-ups (priority-ordered)

### 1. Pin `ubuntu-latest` to `ubuntu-24.04` (small)

Every job below uses `ubuntu-latest`, which now resolves to 26.04 (image
`20261101.186.1`). The last green main run (2026-10-21) used 24.04
(`20261018.339.1`).

- `ci.yml`: `changes`, `docs`
- `test.yml`: `unit`, `synth-onramp`, `cosim-cpu`, `opensta-to-ir-tests`,
  `lint`, `benchmark`, `prepare-mcu-soc-jtir`, `cvc-reference`,
  `timing-comparison`, `mcu-soc-cvc`, `mcu-soc-comparison`,
  `backend-equivalence`
- `build.yml`: the `cpu` matrix entry (`runner: ubuntu-latest`, line 52). The
  hip entry also says `ubuntu-latest` but builds inside
  `rocm/dev-ubuntu-24.04`, so its host image matters less. Pin it anyway for
  consistency.
- `mcu-soc-rebuild.yml` `rebuild`, `publish-netlist-graph.yml` (3 jobs),
  `release.yml` `bump-tap` and `docs-version`. These don't compile
  mt-kahypar, but pinning all of them keeps the image a deliberate choice.

#237 only touched docs, so the `changes` filter skipped the compile jobs and
Documentation (which runs `cargo doc`) was the only job to hit this. The next
code PR will fail in every Linux compile job until this lands.

### 2. Make mt-kahypar build on 26.04 (medium, upstream)

The failing header is `mt-kahypar-sc/mt-kahypar/utils/memory_tree.h` in the
`mt-kahypar` 0.2.0 crate (crates.io, repo `gzz2000/mt-kahypar-rs`). It uses
`uint8_t` for `enum class OutputType : uint8_t` but only includes `<map>`,
`<string>` and `<memory>`. The newer GCC no longer provides `<cstdint>`
through those, so the compiler reports `found ':' in nested-name-specifier`.

133 files in the vendored `mt-kahypar-sc` use fixed-width integer types without
including `<cstdint>` directly. Most get it from another include today, so the
real failure set is unknown until a 26.04 build runs past `memory_tree.h`.
Reproduce in an `ubuntu:26.04` container with `cargo doc --no-deps --lib`.
The fix is `#include <cstdint>` where it's missing, upstream in mt-kahypar,
then a crate bump. If upstream is slow, carry it on a fork per
`~/.claude/FORKED_DEPS_WORKFLOW.md`. Once it builds, moving off the 24.04 pin is
a separate, deliberate change.

### 3. Small leftovers

- Remove the `~/Code/jacquard-handoff-resolve` worktree once #237 merges.
- `~/Code/jacquard-migration-part2` is an orphan directory: its worktree was
  removed, but a stale `.git` file and a `.tldr` cache came back. Delete it
  after a look.
- `docs/plans/ci-build-test-split.md` is a completed plan that is neither in
  `SUMMARY.md` nor archived. Move it with
  `python3 docs/scripts/move_doc.py docs/plans/ci-build-test-split.md docs/plans/archive/`
  and add it under "Completed plans".

## Critical context

- The Documentation failure on #237 is in the "Build cargo docs" step, not in
  mdBook or the link check. Locally `mdbook build` and `check_doc_links.py` both
  pass on #237.
- Failed run: <https://github.com/gpu-eda/Jacquard/actions/runs/43219778104>.
- A run waiting on first-contributor approval keeps the workflow file from when
  it was created. That's why #235's first run still hit the mdbook-d2 cache bug
  after #236 merged.

## References

- [`docs/handoff-discipline.md`](../handoff-discipline.md) — how to resolve this handoff
- [`docs/development.md`](../development.md) — the landing rule (merge queue, rebase-only)
- `.github/actions/setup-sccache/action.yml` — the runner-OOM context for mt-kahypar's `-O3` build

---

**Resume in a new session with:**
```
cd ~/Code/jacquard-ci-ubuntu-pin
/resume_handoff docs/handoffs/ci-ubuntu-26-handoff.md
```
