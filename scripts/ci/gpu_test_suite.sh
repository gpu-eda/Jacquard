#!/usr/bin/env bash
# Full GPU functional test suite, backend/runner-agnostic.
#
# Runs a *prebuilt* jacquard binary (JACQUARD_BIN) through the sim / X-prop /
# timed / cosim paths. Shared by the `cuda` (GitHub-hosted T4) and
# `cuda-blackwell` (self-hosted sm_120) jobs so the two stay in lockstep by
# construction — the only difference between those jobs is which GPU they run on.
#
# Usage:
#   JACQUARD_BIN=target/release/jacquard bash scripts/ci/gpu_test_suite.sh <prefix>
#     <prefix>  label baked into output filenames, e.g. `cuda` or `blackwell`
#
# Requires on PATH: python3 (stdlib only), jq, and the built binary's GPU
# runtime (cudart + driver).
set -euo pipefail

: "${JACQUARD_BIN:?JACQUARD_BIN is required (path to a prebuilt jacquard)}"
PREFIX="${1:?output prefix required (e.g. cuda)}"

TT=tests/timing_test
IC="$TT/inv_chain_pnr"

echo "::group::[$PREFIX] timing-test sim"
time ( "$JACQUARD_BIN" sim \
  "$TT/dff_test_synth.gv" "$TT/dff_test.vcd" \
  "$TT/ci_${PREFIX}_output.vcd" 1 ) 2>&1 | tee "${PREFIX}_timing.txt"
echo "::endgroup::"

if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  {
    echo "## ${PREFIX} simulation performance"
    echo '```'
    cat "${PREFIX}_timing.txt"
    echo '```'
  } >> "$GITHUB_STEP_SUMMARY"
fi

echo "::group::[$PREFIX] X-propagation sim"
"$JACQUARD_BIN" sim \
  "$TT/dff_test_synth.gv" "$TT/dff_test.vcd" \
  "$TT/ci_${PREFIX}_xprop_output.vcd" 1 --xprop 2>&1 | tee "${PREFIX}_xprop.txt"
# FATAL: a missing X means the seed/X-aware path silently regressed.
if grep -q 'x' "$TT/ci_${PREFIX}_xprop_output.vcd"; then
  echo "X-propagation VCD contains X values as expected"
else
  echo "ERROR: No X values in ${PREFIX} xprop output VCD (X-propagation broken)"
  exit 1
fi
echo "::endgroup::"

# #104: exercise the timed path (timed launcher + EventBuffer drain + report
# finalize). Arrival/violation logic is shared kernel code; this asserts the
# per-backend Rust wiring runs and emits a structurally valid report.
echo "::group::[$PREFIX] timed path + timing report (inv_chain_pnr, #104)"
"$JACQUARD_BIN" sim \
  "$IC/inv_chain.v" "$IC/inv_chain_stimulus.vcd" \
  "$IC/jacquard_timed_output_${PREFIX}.vcd" 1 \
  --timing-ir "$IC/inv_chain.jtir" \
  --timed \
  --timing-report "$IC/timing_report_${PREFIX}.json" \
  --timing-summary 2>&1 | tee "${PREFIX}_timed.txt"
python3 scripts/ci/validate_timing_report.py "$IC/timing_report_${PREFIX}.json"
echo "::endgroup::"

# Phase 2 Stage B (#105): logic + GPU-peripheral (UART/bus) cosim fixtures vs
# the committed CPU/Metal goldens.
echo "::group::[$PREFIX] cosim Stage B — logic + UART/bus (#105)"
JACQUARD_BIN="$JACQUARD_BIN" COSIM_SCOPE=all bash scripts/ci/cosim_cpu_check.sh
echo "::endgroup::"

# Phase 2 Stage C (#105): the mcu_soc flash cosim (19.6 MB netlist, ~40s
# partitioning) vs the committed CpuBackend golden. Kept separate from `all`.
echo "::group::[$PREFIX] cosim Stage C — flash (#105)"
JACQUARD_BIN="$JACQUARD_BIN" COSIM_SCOPE=flash bash scripts/ci/cosim_cpu_check.sh
echo "::endgroup::"

# QSPI PSRAM: writable RAM-mode flash peripheral (enter-QPI / quad-write /
# quad-read round-trip). Tiny AIGPDK netlist, so cheap — but kept as its own
# scope alongside the flash fixture it extends.
echo "::group::[$PREFIX] cosim — QSPI PSRAM (RAM-mode flash)"
JACQUARD_BIN="$JACQUARD_BIN" COSIM_SCOPE=qspi bash scripts/ci/cosim_cpu_check.sh
echo "::endgroup::"

mkdir -p target/test-out

# ADR 0021 behavioral on-ramp: feed the *behavioral* dff_test.v to `sim` — it
# must detect RTL, synthesize via the embedded YoWASP Yosys, and produce waves
# bit-identical to the gate-level fixture run at the top of this suite. Needs the
# synth feature (all release/CI binaries carry it). Historically Metal-only.
echo "::group::[$PREFIX] behavioral RTL on-ramp smoke (ADR 0021)"
"$JACQUARD_BIN" sim \
  "$TT/dff_test.v" "$TT/dff_test.vcd" \
  "$TT/onramp_${PREFIX}_output.vcd" 1 --emit-synth "$TT/onramp_${PREFIX}_synth.gv"
if diff <(grep '^[01]!' "$TT/ci_${PREFIX}_output.vcd") \
        <(grep '^[01]!' "$TT/onramp_${PREFIX}_output.vcd"); then
  echo "on-ramp waves are bit-identical to the gate-level fixture run"
else
  echo "ERROR: on-ramp output diverged from the gate-level fixture run"
  exit 1
fi
echo "::endgroup::"

# X-propagation via the `sim` path (#95): xprop_demo under --xprop and 2-state,
# each validated by the property checker (distinct from the cosim goldens, which
# check the cosim path). Historically Metal-only.
echo "::group::[$PREFIX] X-propagation sim — xprop_demo (#95)"
XD=tests/xprop_cosim
"$JACQUARD_BIN" sim "$XD/xprop_demo_synth.gv" "$XD/xprop_demo_stim.vcd" \
  "target/test-out/ci_${PREFIX}_sim_xprop.vcd" 1 --xprop --check-with-cpu
python3 "$XD/check.py" "target/test-out/ci_${PREFIX}_sim_xprop.vcd" xprop
"$JACQUARD_BIN" sim "$XD/xprop_demo_synth.gv" "$XD/xprop_demo_stim.vcd" \
  "target/test-out/ci_${PREFIX}_sim_2state.vcd" 1
python3 "$XD/check.py" "target/test-out/ci_${PREFIX}_sim_2state.vcd" two-state
echo "::endgroup::"

# Register value-injection under --xprop (#108): with and without reg-init, each
# validated by the property checker. Historically Metal-only.
echo "::group::[$PREFIX] register value-injection cosim (#108)"
"$JACQUARD_BIN" cosim "$XD/xprop_demo_synth.gv" \
  --config "$XD/sim_config.json" \
  --output-vcd "target/test-out/ci_${PREFIX}_cosim_noreginit.vcd" \
  --max-clock-edges 200 --xprop
python3 "$XD/check.py" "target/test-out/ci_${PREFIX}_cosim_noreginit.vcd" xprop
"$JACQUARD_BIN" cosim "$XD/xprop_demo_synth.gv" \
  --config "$XD/sim_config_reginit.json" \
  --output-vcd "target/test-out/ci_${PREFIX}_cosim_reginit.vcd" \
  --max-clock-edges 200 --xprop
python3 "$XD/check.py" "target/test-out/ci_${PREFIX}_cosim_reginit.vcd" reg-init
echo "::endgroup::"

echo "[$PREFIX] GPU test suite passed."
