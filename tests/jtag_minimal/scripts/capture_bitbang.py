#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""
capture_bitbang.py — cocotb harness that records OpenOCD's
remote_bitbang byte stream during an Icarus RTL simulation of
tests/jtag_minimal/rtl/top.sv.

What this does:

  1. Opens a local TCP server speaking OpenOCD's remote_bitbang
     protocol. Every byte that OpenOCD sends is tee'd to
     `data/bitbang.rec` for replay by `jacquard cosim --jtag-replay`.
  2. Starts the top-level clock + reset on the DUT.
  3. Pulses TRST_N (1 → 0 → 1) via the harness, and emits the
     equivalent `'r'`/`'u'` remote_bitbang bytes into the capture
     file so the recorded stream is self-contained (OpenOCD's riscv
     driver uses 5×TMS=1 for TAP reset and never emits TRST commands
     itself).
  4. Spawns OpenOCD with the canonical config (`openocd.cfg` in this
     directory) + the magic-write command. OpenOCD performs:
       - TAP enumeration (reads IDCODE = 0xdeadbeef)
       - DMI init (sets DMCONTROL.dmactive = 1)
       - DMI write to DATA0 = 0xCAFEBABE
       - shutdown
  5. Asserts:
       - `dmactive_obs` reached 1 at some point during the run
       - `data0_obs` == 0xCAFEBABE at end of OpenOCD shutdown

Output:
  - `tests/jtag_minimal/data/bitbang.rec` — captured byte stream

Run with `make tests/jtag_minimal/data/bitbang.rec` (Makefile target
forthcoming) or directly via `python3 capture_bitbang.py`.
"""

import logging
import os
import socket
import subprocess
import threading
from pathlib import Path

import cocotb
from cocotb.clock import Clock
from cocotb.triggers import ClockCycles, RisingEdge
from cocotb_tools.runner import get_runner

# Tunables. The default port is high enough to avoid collisions
# with the host's local OpenOCD installs (if any), and is overridable
# via OPENOCD_REMOTE_BITBANG_PORT for parallel CI jobs.
DEFAULT_PORT = int(os.getenv("OPENOCD_REMOTE_BITBANG_PORT", "9824"))
HOLD_CYCLES = 4   # chip-clock cycles per JTAG drive (matches Jacquard's --jtag-hold-cycles 8 = 2x edges/cycle * 4 cycles)

THIS_DIR = Path(__file__).resolve().parent
TEST_DIR = THIS_DIR.parent
DATA_DIR = TEST_DIR / "data"
DATA_DIR.mkdir(parents=True, exist_ok=True)
CAPTURE_PATH = DATA_DIR / "bitbang.rec"


class RemoteBitbangServer:
    """Tiny TCP server speaking OpenOCD's remote_bitbang protocol.

    Each ASCII command from OpenOCD ('0'-'7' for (TCK,TMS,TDI) drives,
    'r'/'s'/'t'/'u' for TRST/SRST, 'R' for TDO read, 'B'/'b' for blink
    no-ops) is enqueued; the cocotb pad pump pops one per pump tick
    and applies to the DUT. 'R' replies are sent from the most-recent
    sampled TDO.

    Every inbound byte is also tee'd to `capture_path` so the stream
    can be replayed by Jacquard's JtagReplayModel.
    """

    QUEUE_LIMIT = 256

    def __init__(self, port: int, capture_path: Path):
        import collections
        self.port = port
        self._lock = threading.Lock()
        self._cmd_q = collections.deque()
        self._cmd_room = threading.Event()
        self._cmd_room.set()
        # Drive state shadowed for the pad pump.
        self.tck = 0
        self.tms = 0
        self.tdi = 0
        self.trst_n = 1
        self.tdo = 0
        self._tx = bytearray()       # outgoing 'R' replies
        self._sock = None
        self._client = None
        self._thread = None
        self._running = False
        self._connected = threading.Event()
        self._quit = threading.Event()
        self.log = logging.getLogger("rbb")
        self.log.setLevel(logging.INFO)
        self._capture = capture_path.open("wb")
        self.log.info("capture stream → %s", capture_path)

    def start(self):
        self._sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self._sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._sock.bind(("127.0.0.1", self.port))
        self._sock.listen(1)
        self._running = True
        self._thread = threading.Thread(
            target=self._serve, name="rbb-server", daemon=True
        )
        self._thread.start()
        self.log.info("remote_bitbang server listening on :%d", self.port)

    def _serve(self):
        try:
            self._client, addr = self._sock.accept()
            self._client.setblocking(True)
            self._client.settimeout(0.05)
            self.log.info("OpenOCD connected from %s", addr)
            self._connected.set()
            while self._running:
                self._cmd_room.wait(timeout=0.05)
                with self._lock:
                    if self._tx:
                        try:
                            n = self._client.send(bytes(self._tx))
                            del self._tx[:n]
                        except (BlockingIOError, OSError, socket.timeout):
                            pass
                try:
                    chunk = self._client.recv(4096)
                    if not chunk:
                        break
                except (BlockingIOError, socket.timeout):
                    chunk = b""
                except OSError:
                    break
                if chunk:
                    self._capture.write(chunk)
                    self._capture.flush()
                for byte in chunk:
                    self._handle_byte(byte)
        finally:
            self._quit.set()
            try:
                if self._client:
                    self._client.close()
            except OSError:
                pass

    def _handle_byte(self, b: int):
        c = chr(b)
        if c in "01234567":
            with self._lock:
                self._cmd_q.append(("drive", (b >> 2) & 1, (b >> 1) & 1, b & 1))
                if len(self._cmd_q) >= self.QUEUE_LIMIT:
                    self._cmd_room.clear()
        elif c == "R":
            # Queue inline with drives so the pump samples TDO at the
            # cycle the 'R' is processed (otherwise we'd reply with a
            # stale value sampled before the latest drive propagated).
            with self._lock:
                self._cmd_q.append(("read", 0, 0, 0))
        elif c in "rstu":
            with self._lock:
                self.trst_n = 1 if c in "tu" else 0
        elif c in "Bb":
            pass  # blink — no-op
        elif c == "Q":
            self.log.info("OpenOCD sent Quit")
            self._running = False

    def stop(self):
        self._running = False
        try:
            if self._client:
                self._client.shutdown(socket.SHUT_RDWR)
                self._client.close()
        except OSError:
            pass
        try:
            if self._sock:
                self._sock.close()
        except OSError:
            pass
        self._capture.close()
        self._quit.set()

    def pop_cmd(self):
        """Pop next command. Returns ('drive', tck, tms, tdi) or
        ('read', _, _, _) or None if queue empty."""
        with self._lock:
            if not self._cmd_q:
                return None
            kind, a, b, c = self._cmd_q.popleft()
            if len(self._cmd_q) < self.QUEUE_LIMIT // 2:
                self._cmd_room.set()
            if kind == "drive":
                self.tck, self.tms, self.tdi = a, b, c
            return (kind, a, b, c)

    def push_tdo_reply(self, bit: int):
        with self._lock:
            self._tx.append(ord("0") + (bit & 1))

    def wait_connected(self, timeout_s: float) -> bool:
        return self._connected.wait(timeout=timeout_s)


async def jtag_pad_pump(dut, server: RemoteBitbangServer, hold_cycles: int):
    """One JTAG cmd per `hold_cycles` chip-clock posedges.

    'drive' commands set (tck, tms, tdi); 'read' commands sample tdo
    and push the bit back to OpenOCD. Both kinds advance the pump
    by one hold window. TRST_N is driven by the harness (`trst_pulse`)
    directly — pad pump doesn't touch it.
    """
    while True:
        await RisingEdge(dut.clk)
        cmd = server.pop_cmd()
        if cmd is not None:
            kind, tck, tms, tdi = cmd
            if kind == "drive":
                dut.tck.value = tck
                dut.tms.value = tms
                dut.tdi.value = tdi
            elif kind == "read":
                try:
                    tdo_bit = int(dut.tdo.value)
                except ValueError:
                    tdo_bit = 0
                server.push_tdo_reply(tdo_bit)
        for _ in range(hold_cycles - 1):
            await RisingEdge(dut.clk)


async def trst_pulse(dut, server: RemoteBitbangServer, hold_cycles: int = 30):
    """Pulse TRST_N 1→0→1 directly. Tees `'r'`/`'u'` bytes into the
    capture file so the recorded stream reproduces the pulse when
    replayed through Jacquard's JtagReplayModel."""
    dut.trst_n.value = 0
    server._capture.write(b"r")
    server._capture.flush()
    await ClockCycles(dut.clk, hold_cycles)
    dut.trst_n.value = 1
    server._capture.write(b"u")
    server._capture.flush()
    await ClockCycles(dut.clk, 10)


@cocotb.test()
async def capture(dut):
    log = logging.getLogger("capture")
    log.setLevel(logging.INFO)

    if CAPTURE_PATH.exists():
        CAPTURE_PATH.unlink()

    server = RemoteBitbangServer(port=DEFAULT_PORT, capture_path=CAPTURE_PATH)
    server.start()

    # Initial pin state. TRST_N starts HIGH (deasserted) so the
    # subsequent harness-driven 1→0→1 pulse in trst_pulse() gives the
    # DTM's `negedge trst_n` async-reset clauses a clean negedge to
    # fire on (Icarus does not treat X→0 as a negedge).
    dut.tck.value = 0
    dut.tms.value = 0
    dut.tdi.value = 0
    dut.trst_n.value = 1
    dut.rst_n.value = 0

    cocotb.start_soon(Clock(dut.clk, 40, unit="ns").start())  # 25 MHz
    cocotb.start_soon(jtag_pad_pump(dut, server, hold_cycles=HOLD_CYCLES))

    await ClockCycles(dut.clk, 20)
    await trst_pulse(dut, server)
    dut.rst_n.value = 1
    await ClockCycles(dut.clk, 10)

    # Spawn OpenOCD.
    openocd_cfg = THIS_DIR / "openocd.cfg"
    openocd_log = DATA_DIR / "openocd.log"
    cmd = [
        "openocd",
        "-f", str(openocd_cfg),
        "-c", f"remote_bitbang port {DEFAULT_PORT}",
        "-c", "init",
        "-c", "riscv dmi_write 0x04 0xcafebabe",
        "-c", "shutdown",
    ]
    log.info("Spawning OpenOCD: %s", " ".join(cmd))
    proc = subprocess.Popen(
        cmd,
        stdout=open(openocd_log, "wb"),
        stderr=subprocess.STDOUT,
    )
    if not server.wait_connected(timeout_s=30.0):
        proc.terminate()
        server.stop()
        raise AssertionError("OpenOCD did not connect within 30 s")
    log.info("OpenOCD connected; waiting for shutdown")

    # Run cycles in chunks until OpenOCD finishes or we hit the budget.
    cycle_budget = 1_000_000
    chunk = 50_000
    burned = 0
    while burned < cycle_budget:
        await ClockCycles(dut.clk, chunk)
        burned += chunk
        if proc.poll() is not None:
            break
    rc = proc.poll()
    if rc is None:
        log.warning("OpenOCD still running after %d cycles; terminating", burned)
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
        rc = proc.returncode
    else:
        log.info("OpenOCD finished naturally after %d cycles (rc=%d)", burned, rc)

    # Drain remaining JTAG queue.
    await ClockCycles(dut.clk, 1_000)

    # Read observable outputs.
    dmactive_v = int(dut.dmactive_obs.value)
    haltreq_v = int(dut.haltreq_obs.value)
    data0_v = int(dut.data0_obs.value)
    log.info("dmactive_obs = %d  haltreq_obs = %d  data0_obs = 0x%08x",
             dmactive_v, haltreq_v, data0_v)

    server.stop()

    assert dmactive_v == 1, (
        f"dmactive_obs did not become 1 — DMI init failed "
        f"(see {openocd_log})"
    )
    assert data0_v == 0xCAFEBABE, (
        f"data0_obs = 0x{data0_v:08x} != 0xCAFEBABE — DMI write to DATA0 "
        f"didn't land (see {openocd_log})"
    )
    log.info("Capture complete — %d bytes recorded at %s",
             CAPTURE_PATH.stat().st_size, CAPTURE_PATH)


# ----------------------------------------------------------------------------
# Cocotb runner — Icarus on the vendored Hazard3 sources + top.sv
# ----------------------------------------------------------------------------

if __name__ == "__main__":
    rtl_dir = (TEST_DIR / "rtl").resolve()
    sources = [
        str(rtl_dir / "hazard3_jtag_dtm_core.v"),
        str(rtl_dir / "hazard3_jtag_dtm.v"),
        str(rtl_dir / "hazard3_apb_async_bridge.v"),
        str(rtl_dir / "hazard3_sync_1bit.v"),
        str(rtl_dir / "hazard3_dm.v"),
        str(rtl_dir / "top.sv"),
    ]
    runner = get_runner("icarus")
    runner.build(
        verilog_sources=sources,
        includes=[str(rtl_dir)],
        hdl_toplevel="top",
        build_dir=str(THIS_DIR / "sim_build"),
        waves=True,
    )
    runner.test(
        hdl_toplevel="top",
        test_module="capture_bitbang",
        build_dir=str(THIS_DIR / "sim_build"),
        waves=True,
    )
