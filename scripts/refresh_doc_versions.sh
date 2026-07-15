#!/usr/bin/env bash
#
# Regenerate versions.json at the root of gh-pages, which is what the docs
# version picker reads (theme/version-picker.js).
#
# The list is derived from the version directories that are actually published,
# not accumulated across runs: prune a directory and it leaves the picker on the
# next run, so the control can never offer a version that 404s. Run it after a
# deploy, from both the main-branch docs job and the per-release docs job — it
# is idempotent, and doing it in both places means the list is correct whether
# the last event was a merge or a tag.
#
# Needs: GITHUB_TOKEN with contents:write, and GITHUB_REPOSITORY (both are
# already present in Actions; GITHUB_TOKEN must be mapped in explicitly).

set -euo pipefail

: "${GITHUB_TOKEN:?GITHUB_TOKEN is required}"
: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

git clone --quiet --depth 1 --branch gh-pages \
  "https://x-access-token:${GITHUB_TOKEN}@github.com/${GITHUB_REPOSITORY}.git" "$tmp"
cd "$tmp"

# Final releases only, newest first. Release candidates publish docs too — their
# release notes link into them — but listing every -rc would crowd out the
# releases anyone is actually looking for.
#
# `v[0-9]*/` rather than `v*/`: it must agree with the /^v\d/ test the picker
# uses to decide it is on a pinned build, or a future directory like vendor/
# would be offered as a version.
#
# `|| true`: no version directories yet is a valid state (an empty list), not a
# failure, and pipefail would otherwise turn the glob miss into one.
{ ls -d v[0-9]*/ 2>/dev/null || true; } \
  | sed 's#/##' \
  | { grep -vE -- '-rc\.' || true; } \
  | sort -Vr \
  | jq -R . \
  | jq -s . > versions.json

git config user.name "jacquard-release-bot"
git config user.email "noreply@github.com"
git add versions.json

if git diff --cached --quiet; then
  echo "version list already correct: $(tr -d '\n ' < versions.json)"
  exit 0
fi

git commit --quiet -m "docs: refresh version list"
git push --quiet
echo "published version list: $(tr -d '\n ' < versions.json)"
