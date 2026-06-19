#!/usr/bin/env bash
# Cosim regression check (backend-agnostic).
#
# Runs `jacquard cosim` regression fixtures with a given build and diffs each
# output against the committed expected golden in `tests/<fixture>/expected/`.
# Those goldens were captured cross-backend: the CpuBackend outputs are
# byte-identical to the Metal golden (Phase 1, #105), so any backend's output
# must match them too.
#
# Backend selection is via the binary (JACQUARD_BIN) — the same goldens gate
# CpuBackend (no features), CUDA, HIP, and Metal.
#
#   cargo build -r --bin jacquard            # no features -> CpuBackend
#   bash scripts/ci/cosim_cpu_check.sh
#   JACQUARD_BIN=target/release/jacquard COSIM_SCOPE=logic bash scripts/ci/cosim_cpu_check.sh
#
# Env:
#   JACQUARD_BIN   path to the jacquard binary (default target/release/jacquard)
#   COSIM_SCOPE    `all` (default) runs all 7 fixtures; `logic` runs only the
#                  4 peripheral-free xprop_cosim fixtures. `logic` is for
#                  backends that don't yet have GPU peripherals (CUDA/HIP
#                  Phase 2 Stage A): the design step is validated without
#                  flash/UART/bus decode.
set -euo pipefail

BIN="${JACQUARD_BIN:-target/release/jacquard}"
SCOPE="${COSIM_SCOPE:-all}"
if [ ! -x "$BIN" ]; then
    echo "error: jacquard binary not found at '$BIN'" >&2
    echo "       build it first: cargo build -r --bin jacquard" >&2
    exit 2
fi
case "$SCOPE" in
    all|logic) ;;
    *) echo "error: COSIM_SCOPE must be 'all' or 'logic' (got '$SCOPE')" >&2; exit 2 ;;
esac

OUT="$(mktemp -d)"
trap 'rm -rf "$OUT"' EXIT
mkdir -p target/test-out

fails=0
total=0
pass() { echo "PASS  $1"; }
fail() { echo "FAIL  $1"; fails=$((fails + 1)); }

# Run a fixture command; mark fixture as failed if the run itself errors.
run() {
    local label="$1"; shift
    if ! "$@"; then
        echo "  (fixture '$label' command exited non-zero)" >&2
        return 1
    fi
}

# diff an actual output against its committed expected golden.
check() {
    local label="$1" actual="$2" expected="$3"
    total=$((total + 1))
    if [ ! -f "$actual" ]; then
        fail "$label (no output produced at $actual)"
        return
    fi
    if diff -q "$expected" "$actual" >/dev/null 2>&1; then
        pass "$label"
    else
        fail "$label"
        echo "  expected: $expected" >&2
        echo "  actual:   $actual" >&2
        diff "$expected" "$actual" >&2 || true
    fi
}

# --- xprop_cosim: xprop / 2state / reg-init variants (peripheral-free) ----
run xprop_xprop "$BIN" cosim tests/xprop_cosim/xprop_demo_synth.gv \
    --config tests/xprop_cosim/sim_config.json --top-module xprop_demo \
    --max-clock-edges 40 --xprop --output-vcd "$OUT/xprop.vcd" || true
check xprop "$OUT/xprop.vcd" tests/xprop_cosim/expected/xprop.vcd

run xprop_2state "$BIN" cosim tests/xprop_cosim/xprop_demo_synth.gv \
    --config tests/xprop_cosim/sim_config.json --top-module xprop_demo \
    --max-clock-edges 40 --output-vcd "$OUT/2state.vcd" || true
check 2state "$OUT/2state.vcd" tests/xprop_cosim/expected/2state.vcd

run xprop_noreg "$BIN" cosim tests/xprop_cosim/xprop_demo_synth.gv \
    --config tests/xprop_cosim/sim_config.json \
    --output-vcd "$OUT/noreginit.vcd" --max-clock-edges 100 || true
check noreginit "$OUT/noreginit.vcd" tests/xprop_cosim/expected/noreginit.vcd

run xprop_reg "$BIN" cosim tests/xprop_cosim/xprop_demo_synth.gv \
    --config tests/xprop_cosim/sim_config_reginit.json \
    --output-vcd "$OUT/reginit.vcd" --max-clock-edges 100 || true
check reginit "$OUT/reginit.vcd" tests/xprop_cosim/expected/reginit.vcd

# --- peripheral fixtures (flash/UART/bus) — `all` scope only -------------
if [ "$SCOPE" = all ]; then
    # dual_uart: events JSON lands at a fixed path
    run dual_uart "$BIN" cosim tests/dual_uart/dual_uart_synth.gv \
        --config tests/dual_uart/sim_config.json --top-module dual_uart_top \
        --max-clock-edges 10000 || true
    cp -f target/test-out/dual_uart_events.json "$OUT/dual_uart_events.json" 2>/dev/null || true
    check dual_uart_events "$OUT/dual_uart_events.json" tests/dual_uart/expected/dual_uart_events.json

    # apb_trace: bus-trace CSV (2-state + xprop)
    run apb_trace "$BIN" cosim tests/apb_trace/apb_trace_synth.gv \
        --config tests/apb_trace/sim_config.json --top-module apb_trace \
        --max-clock-edges 200 --bus-trace-csv "$OUT/apb_trace.csv" || true
    check apb_trace "$OUT/apb_trace.csv" tests/apb_trace/expected/apb_trace.csv

    run apb_trace_x "$BIN" cosim tests/apb_trace/apb_trace_synth.gv \
        --config tests/apb_trace/sim_config.json --top-module apb_trace \
        --max-clock-edges 200 --xprop --bus-trace-csv "$OUT/apb_trace_xprop.csv" || true
    check apb_trace_xprop "$OUT/apb_trace_xprop.csv" tests/apb_trace/expected/apb_trace_xprop.csv
fi

echo
if [ "$fails" -ne 0 ]; then
    echo "=== $fails of $total cosim fixture(s) FAILED ($BIN, scope=$SCOPE) ==="
    exit 1
fi
echo "=== all $total cosim fixtures PASS ($BIN == committed golden, scope=$SCOPE) ==="
