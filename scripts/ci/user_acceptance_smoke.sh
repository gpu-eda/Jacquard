#!/usr/bin/env bash
# User-acceptance smoke test (issue: distribution / ADR 0018).
#
# Runs the *documented* install-verification flow against an already-installed
# (relocated) binary — i.e. what a user gets from `brew install`, `cargo
# binstall`, or the prebuilt release tarball, NOT a from-source build tree.
# Mirrors docs/installation.md § Verify, plus a `sim` run (the path that shipped
# broken in v0.2.0 because the relocated smoke only exercised `cosim`).
#
# This is the shared logic behind the release gate (release.yml) and the
# standalone user-acceptance workflow (user-acceptance.yml). It deliberately
# uses ONLY the binaries in BIN_DIR and the checked-in fixtures under REPO — it
# must not touch the cargo build tree, so it catches relocatability regressions
# (e.g. a kernel loaded from a compile-time path).
#
# Usage: user_acceptance_smoke.sh <BIN_DIR> [REPO_ROOT]
#   BIN_DIR    dir containing the installed `jacquard` + `opensta-to-ir`
#   REPO_ROOT  checkout providing tests/ fixtures (default: $PWD)
set -euo pipefail

BIN_DIR="${1:?usage: user_acceptance_smoke.sh <BIN_DIR> [REPO_ROOT]}"
REPO="${2:-$PWD}"
JACQUARD="$BIN_DIR/jacquard"
OPENSTA="$BIN_DIR/opensta-to-ir"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

pass() { printf '  \033[32mPASS\033[0m %s\n' "$1"; }
step() { printf '\n=== %s ===\n' "$1"; }

[ -x "$JACQUARD" ] || { echo "error: no jacquard at $JACQUARD" >&2; exit 2; }

# 1. --version (docs/installation.md § Verify, line 1)
step "jacquard --version"
"$JACQUARD" --version
pass "version reported"

# 2. opensta-to-ir present (ships alongside jacquard, installation.md)
step "opensta-to-ir --help"
"$OPENSTA" --help >/dev/null
pass "opensta-to-ir runs"

# 3. sim — the documented quickstart (usage.md). Proves the Metal kernel is
#    embedded (relocatable): a build-tree-path load would fail here.
step "jacquard sim (relocated)"
"$JACQUARD" sim \
    "$REPO/tests/timing_test/dff_test_synth.gv" \
    "$REPO/tests/timing_test/dff_test.vcd" \
    "$WORK/sim_out.vcd" 1 --max-clock-edges 5
[ -s "$WORK/sim_out.vcd" ] || { echo "error: sim produced no output VCD" >&2; exit 1; }
pass "sim ran + wrote output VCD"

# 4. cosim — the exact docs/installation.md § Verify command (lines 92-95).
step "jacquard cosim apb_trace (docs Verify)"
"$JACQUARD" cosim \
    "$REPO/tests/apb_trace/apb_trace_synth.gv" \
    --config "$REPO/tests/apb_trace/sim_config.json" \
    --top-module apb_trace --max-clock-edges 200 \
    --bus-trace-csv "$WORK/apb.csv"
python3 "$REPO/tests/apb_trace/check.py" "$WORK/apb.csv"
pass "cosim ran + APB bus trace verified"

printf '\n\033[32mUser-acceptance smoke passed\033[0m (version, opensta-to-ir, sim, cosim).\n'
