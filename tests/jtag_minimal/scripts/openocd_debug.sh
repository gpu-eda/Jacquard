#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Drive a running `jacquard cosim --jtag-server <PORT>` with REAL OpenOCD
# and assert the debug path works end-to-end (V2 in
# docs/plans/jtag-debug-server.md).
#
# Unlike the replay / bitbang_client equivalence gate — which reaches
# `data0_obs == 0xCAFEBABE` via a DMI *write* and never depends on a
# correct TDO *read* — this examines the DTM (which reads IDCODE /
# DTMCS / DMSTATUS back over TDO) and then writes *and reads back* DATA0.
# It therefore guards the live TDO read path, which only real OpenOCD
# exercises.
#
# Asserts:
#   * TAP IDCODE reads 0xdeadbeef  (scan-chain interrogation over TDO)
#   * `riscv dmi_read 0x04` returns 0xcafebabe  (DMI read-back over TDO)
#
# The jtag_minimal fixture has a *fake* hart (no real CPU), so full RISC-V
# target examination stops at "Failed to read MISA" and OpenOCD exits
# non-zero — exactly as the capture flow expects. We tolerate that exit
# and assert on the two specifics above; DMI access is the contract.
set -uo pipefail

PORT="${1:?usage: openocd_debug.sh <port> [openocd.cfg]}"
HERE="$(cd "$(dirname "$0")" && pwd)"
CFG="${2:-$HERE/openocd.cfg}"
LOG="${OPENOCD_LOG:-/tmp/openocd_debug_${PORT}.log}"
# Seconds to wait for the server to start listening (cosim setup —
# netlist parse + partitioning — runs before it binds).
WAIT_LISTEN="${WAIT_LISTEN:-240}"

# OpenOCD's remote_bitbang driver connects exactly once and does not
# retry, so the server MUST be listening first. Poll the LISTEN state
# without opening a connection (a probe connection would be consumed as
# the single v1 client). `lsof` checks the listening socket directly.
echo "waiting up to ${WAIT_LISTEN}s for 127.0.0.1:${PORT} to listen…"
ready=0
for _ in $(seq 1 "$WAIT_LISTEN"); do
  if lsof -nP -iTCP:"$PORT" -sTCP:LISTEN >/dev/null 2>&1; then
    ready=1
    break
  fi
  sleep 1
done
if [ "$ready" != 1 ]; then
  echo "FAIL: nothing listening on 127.0.0.1:${PORT} after ${WAIT_LISTEN}s"
  exit 1
fi

openocd -f "$CFG" \
  -c "remote_bitbang port $PORT" \
  -c "riscv set_command_timeout_sec 60" \
  -c "init" \
  -c "riscv dmi_write 0x04 0xcafebabe" \
  -c "riscv dmi_read 0x04" \
  -c "shutdown" >"$LOG" 2>&1 || true

echo "----- OpenOCD output -----"
cat "$LOG"
echo "--------------------------"

fail=0
if grep -q "tap/device found: 0xdeadbeef" "$LOG"; then
  echo "PASS: TAP IDCODE = 0xdeadbeef (TDO interrogation works)"
else
  echo "FAIL: IDCODE 0xdeadbeef not read — live TDO read-back is broken"
  fail=1
fi
if grep -qiE "^0x0*cafebabe\b" "$LOG"; then
  echo "PASS: DMI read-back of DATA0 = 0xcafebabe (TDO data read-back works)"
else
  echo "FAIL: dmi_read 0x04 did not return 0xcafebabe — TDO data read-back broken"
  fail=1
fi
exit $fail
