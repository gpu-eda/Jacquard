#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pygame-ce>=2.4"]
# ///
# SPDX-License-Identifier: Apache-2.0
"""Live C64 video renderer for the Jacquard cosim signal-streaming tap.

Reads the UNIX-socket byte stream produced by a ``signal_streams`` tap (see
``docs/architecture/decisions/spikes/video-streaming-tap.md``) and renders the C64 raster live in a
window. Spike artifact — intended to move to the c64-tapeout project.

Run cosim with a video tap configured (socket path must match ``--socket``),
then start this renderer; it connects, frames the raster on hsync/vsync edges,
applies the C64 palette, and blits. It auto-reconnects if cosim drops the
client under backpressure.

    uv run c64_video_renderer.py --socket /tmp/c64-video.sock

The bundle signal order MUST match the cosim config. This renderer assumes:

    colorIndex[3], colorIndex[2], colorIndex[1], colorIndex[0], hsync, vsync

packed LSB-first into one byte per sample (bit 0 = colorIndex[3]). If the
picture's colours are permuted, the pad bit-order differs — see the spike's
open question about ``bidir_CORE2PAD`` bit order and adjust ``decode_sample``.
"""

from __future__ import annotations

import argparse
import socket
import struct
import sys
import time

import pygame

# fpga64_rgbcolor 16-colour C64 palette (Pepto approximation). Reconcile
# against fpga64_rgbcolor.vhd for exact on-die values if colours look off.
PALETTE = [
    (0x00, 0x00, 0x00), (0xFF, 0xFF, 0xFF), (0x68, 0x37, 0x2B), (0x70, 0xA4, 0xB2),
    (0x6F, 0x3D, 0x86), (0x58, 0x8D, 0x43), (0x35, 0x28, 0x79), (0xB8, 0xC7, 0x6F),
    (0x6F, 0x4F, 0x25), (0x43, 0x39, 0x00), (0x9A, 0x67, 0x59), (0x44, 0x44, 0x44),
    (0x6C, 0x6C, 0x6C), (0x9A, 0xD2, 0x84), (0x6C, 0x5E, 0xB5), (0x95, 0x95, 0x95),
]
PAL_BYTES = [bytes(c) for c in PALETTE]

# Generous PAL raster bounds (dot clock ~8 MHz, ~504 dots/line, 312 lines).
WIDTH = 520
HEIGHT = 320

# Per-batch frame header: u64 first_tick LE, u32 sample_count LE.
HEADER = struct.Struct("<QI")


def decode_sample(b: int) -> tuple[int, int, int]:
    """Unpack one sample byte into (colorIndex, hsync, vsync).

    Bit order (LSB-first, config order): bit0=colorIndex[3] .. bit3=colorIndex[0],
    bit4=hsync, bit5=vsync. So colorIndex = bit3 | bit2<<1 | bit1<<2 | bit0<<3.
    """
    color = ((b >> 3) & 1) | ((b >> 2) & 1) << 1 | ((b >> 1) & 1) << 2 | (b & 1) << 3
    hsync = (b >> 4) & 1
    vsync = (b >> 5) & 1
    return color, hsync, vsync


class Raster:
    """Reconstructs frames from the pixel + sync stream into a framebuffer."""

    def __init__(self) -> None:
        self.fb = bytearray(WIDTH * HEIGHT * 3)
        self.x = 0
        self.y = 0
        self.prev_h = 0
        self.prev_v = 0

    def push(self, byte: int) -> bool:
        """Consume one sample. Returns True when a frame completes (vsync)."""
        color, hsync, vsync = decode_sample(byte)
        frame_done = False

        # vsync rising → present current frame, restart at top.
        if vsync and not self.prev_v:
            frame_done = True
            self.x = 0
            self.y = 0
        # hsync rising → next line.
        elif hsync and not self.prev_h:
            self.x = 0
            self.y += 1

        self.prev_h = hsync
        self.prev_v = vsync

        if 0 <= self.x < WIDTH and 0 <= self.y < HEIGHT:
            off = (self.y * WIDTH + self.x) * 3
            self.fb[off:off + 3] = PAL_BYTES[color]
        self.x += 1
        return frame_done

    def surface(self) -> pygame.Surface:
        return pygame.image.frombuffer(bytes(self.fb), (WIDTH, HEIGHT), "RGB")

    def clear(self) -> None:
        self.fb[:] = b"\x00" * len(self.fb)


def connect(path: str) -> socket.socket:
    """Block until cosim is listening on the socket, then connect."""
    while True:
        try:
            s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            s.connect(path)
            s.settimeout(0.1)
            print(f"connected to {path}", file=sys.stderr)
            return s
        except (FileNotFoundError, ConnectionRefusedError):
            time.sleep(0.25)


def run(path: str, bytes_per_sample: int, scale: int) -> None:
    pygame.init()
    screen = pygame.display.set_mode((WIDTH * scale, HEIGHT * scale))
    pygame.display.set_caption("C64 cosim video tap")
    raster = Raster()

    sock: socket.socket | None = None
    buf = bytearray()

    def present() -> None:
        frame = pygame.transform.scale(raster.surface(), (WIDTH * scale, HEIGHT * scale))
        screen.blit(frame, (0, 0))
        pygame.display.flip()
        raster.clear()

    running = True
    while running:
        for event in pygame.event.get():
            if event.type == pygame.QUIT:
                running = False
        if not running:
            break

        if sock is None:
            sock = connect(path)
            buf.clear()

        try:
            chunk = sock.recv(65536)
            if not chunk:
                raise ConnectionResetError
            buf.extend(chunk)
        except socket.timeout:
            continue
        except (ConnectionResetError, BrokenPipeError, OSError):
            print("cosim disconnected; reconnecting", file=sys.stderr)
            sock.close()
            sock = None
            continue

        # Parse as many complete frames as the buffer holds.
        while len(buf) >= HEADER.size:
            _first_tick, n_samples = HEADER.unpack_from(buf, 0)
            payload = n_samples * bytes_per_sample
            if len(buf) < HEADER.size + payload:
                break
            start = HEADER.size
            for i in range(n_samples):
                # Low byte of each sample carries the video bundle.
                if raster.push(buf[start + i * bytes_per_sample]):
                    present()
            del buf[: HEADER.size + payload]

    pygame.quit()


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--socket", default="/tmp/c64-video.sock", help="cosim tap socket path")
    ap.add_argument(
        "--bytes-per-sample",
        type=int,
        default=1,
        help="ceil(n_signals / 8); 1 for the 6-signal video bundle",
    )
    ap.add_argument("--scale", type=int, default=2, help="integer upscale factor")
    args = ap.parse_args()
    run(args.socket, args.bytes_per_sample, args.scale)


if __name__ == "__main__":
    main()
