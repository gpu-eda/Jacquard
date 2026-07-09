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
#   COSIM_SCOPE    `all` (default) runs all 7 fixtures (4 xprop_cosim + dual_uart
#                  + apb_trace ±xprop); `logic` runs only the 4 peripheral-free
#                  xprop_cosim fixtures; `flash` runs only the mcu_soc flash
#                  fixture. `logic` validates the design step alone for a backend
#                  before its GPU UART/bus peripherals land (CUDA/HIP used it for
#                  Phase 2 Stage A; Stage B uses `all`). `flash` is a separate
#                  opt-in scope because the 19.6 MB mcu_soc netlist costs ~40s of
#                  partitioning per run — kept out of `all` so the fast UART/bus
#                  fixtures aren't slowed (Stage C, #105).
set -euo pipefail

BIN="${JACQUARD_BIN:-target/release/jacquard}"
SCOPE="${COSIM_SCOPE:-all}"
if [ ! -x "$BIN" ]; then
    echo "error: jacquard binary not found at '$BIN'" >&2
    echo "       build it first: cargo build -r --bin jacquard" >&2
    exit 2
fi
case "$SCOPE" in
    all|logic|flash|qspi) ;;
    *) echo "error: COSIM_SCOPE must be 'all', 'logic', 'flash', or 'qspi' (got '$SCOPE')" >&2; exit 2 ;;
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
# `all` and `logic` run these; `flash`/`qspi` skip straight to their fixture.
if [ "$SCOPE" = all ] || [ "$SCOPE" = logic ]; then
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
fi

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

    # multi_mem: three QSPI flash instances + two on-chip SRAMs, each with an
    # independent backing store (ADR 0013 plural QSPI, Stage B). The DUT reads a
    # distinct byte from each flash (0xA1/0xB2/0xC3) and each SRAM (0xA1/0xB2)
    # and asserts `all_match`. Golden captured on CpuBackend; byte-identical to
    # the Metal N-instance kernels, so it gates CUDA/HIP/CpuBackend alike.
    run multi_mem "$BIN" cosim tests/multi_mem_cosim/multi_mem_dut_synth.gv \
        --config tests/multi_mem_cosim/sim_config.json --top-module multi_mem_dut \
        --output-vcd "$OUT/multi_mem.vcd" || true
    check multi_mem "$OUT/multi_mem.vcd" tests/multi_mem_cosim/expected/multi_mem_cosim.vcd
fi

# --- qspi_psram fixture (writable RAM-mode flash) — `all` + `qspi` scopes ----
# The DUT enters QPI (0x35), quad-writes 0xA5 to 0x000001 (0x38), then quad-reads
# it back (0xEB) and asserts rdata==0xA5. Exercises the writable-store + enter-QPI
# + quad-write extensions of the flash GPU peripheral. Tiny AIGPDK netlist (~160
# cells), so it rides `all` (CpuBackend CI) and also has a dedicated `qspi` scope
# so the GPU test suite can validate it on CUDA/HIP without the full `all` set.
# Golden captured on CpuBackend, byte-identical to the Metal/CUDA/HIP kernels.
# See tests/qspi_psram/README.md.
if [ "$SCOPE" = all ] || [ "$SCOPE" = qspi ]; then
    run qspi_psram "$BIN" cosim tests/qspi_psram/qspi_psram_dut_synth.gv \
        --config tests/qspi_psram/sim_config.json --top-module qspi_psram_dut \
        --max-clock-edges 200 --output-vcd "$OUT/qspi_psram.vcd" || true
    check qspi_psram "$OUT/qspi_psram.vcd" tests/qspi_psram/expected/qspi_psram.vcd
    # Content assertion (write→read round-trip), independent of the byte-diff.
    total=$((total + 1))
    if python3 tests/qspi_psram/check.py "$OUT/qspi_psram.vcd" >/dev/null 2>&1; then
        pass "qspi_psram_content"
    else
        fail "qspi_psram_content"
    fi

    # qspi_shared_bus: two flashes on ONE shared SCK/SIO bus, distinct CS. The
    # master reads addr 0 from each in turn; a deselected flash must present
    # high-Z on the shared MISO (CS-gated din injection) or it clobbers the
    # other's read. Regression for the shared-bus arbitration fix. Golden
    # captured on CpuBackend, byte-identical to the Metal/CUDA/HIP kernels.
    # See tests/qspi_shared_bus/README.md.
    run qspi_shared_bus "$BIN" cosim tests/qspi_shared_bus/shared_bus_dut_synth.gv \
        --config tests/qspi_shared_bus/sim_config.json --top-module shared_bus_dut \
        --max-clock-edges 2000 --output-vcd "$OUT/qspi_shared_bus.vcd" || true
    check qspi_shared_bus "$OUT/qspi_shared_bus.vcd" tests/qspi_shared_bus/expected/qspi_shared_bus.vcd
    # Content assertion (each flash returned its own byte), independent of the diff.
    total=$((total + 1))
    if python3 tests/qspi_shared_bus/check.py "$OUT/qspi_shared_bus.vcd" >/dev/null 2>&1; then
        pass "qspi_shared_bus_content"
    else
        fail "qspi_shared_bus_content"
    fi
fi

# --- flash fixture (mcu_soc) — `flash` scope only ------------------------
# mcu_soc is a whole SoC: the committed 19.6 MB netlist + 3.6 KB self-contained
# firmware (tests/mcu_soc/{data/6_final.v,software.bin}) boot through the SPI
# flash. sim_config_selfcontained.json points flash.firmware at the committed
# software.bin so this runs on a fresh checkout (no chipflow build). 10k edges
# reaches a real flash read (cmd 0x03 @ 0x100000) — the boot sequence is
# deterministic, so the output VCD is byte-identical run-to-run. This golden was
# captured on CpuBackend and is byte-identical to the Metal flash kernel (C2,
# #105), so it gates CUDA/HIP/CpuBackend alike. Cost: ~40s partitioning + ~4s
# sim per run — heavy, hence a dedicated opt-in scope (Stage C, #105).
if [ "$SCOPE" = flash ]; then
    run mcu_flash "$BIN" cosim tests/mcu_soc/data/6_final.v \
        --config tests/mcu_soc/sim_config_selfcontained.json --top-module top \
        --max-clock-edges 10000 --output-vcd "$OUT/mcu_flash.vcd" || true
    check mcu_flash "$OUT/mcu_flash.vcd" tests/mcu_soc/expected/mcu_flash.vcd
fi

echo
if [ "$fails" -ne 0 ]; then
    echo "=== $fails of $total cosim fixture(s) FAILED ($BIN, scope=$SCOPE) ==="
    exit 1
fi
echo "=== all $total cosim fixtures PASS ($BIN == committed golden, scope=$SCOPE) ==="
