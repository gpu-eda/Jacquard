#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Pass criterion for the APB3 bus-trace test (ADR 0013).

Reads the CSV produced by `jacquard cosim --bus-trace-csv` and asserts
the four transactions the `apb_trace` design issues, by content (not by
exact tick): two writes followed by two reads that read back the written
values. Usage: check.py <csv_path>
"""
import csv
import sys


# (dir, addr, data) — order-preserving expectation.
EXPECTED = [
    ("WR", 0x0, 0xCAFEBABE),
    ("WR", 0x4, 0x12345678),
    ("RD", 0x0, 0xCAFEBABE),
    ("RD", 0x4, 0x12345678),
]


def main() -> int:
    if len(sys.argv) != 2:
        print(f"usage: {sys.argv[0]} <bus_trace.csv>", file=sys.stderr)
        return 2
    path = sys.argv[1]

    rows = []
    with open(path, newline="") as f:
        for row in csv.DictReader(f):
            rows.append(row)

    txns = [
        (r["dir"], int(r["addr"], 0), int(r["data"], 0), r["resp"])
        for r in rows
        if r["bus"] == "apb0"
    ]

    print(f"Decoded {len(txns)} apb0 transaction(s):")
    for dirn, addr, data, resp in txns:
        print(f"  {dirn} 0x{addr:X} {'<=' if dirn == 'WR' else '=>'} 0x{data:08X} [{resp}]")

    ok = len(txns) == len(EXPECTED)
    for got, want in zip(txns, EXPECTED):
        gd, ga, gv, gresp = got
        wd, wa, wv = want
        if (gd, ga, gv) != (wd, wa, wv) or gresp != "OK":
            ok = False
            print(
                f"MISMATCH: got {gd} 0x{ga:X} 0x{gv:08X} [{gresp}], "
                f"want {wd} 0x{wa:X} 0x{wv:08X} [OK]",
                file=sys.stderr,
            )

    if ok:
        print("PASS: APB3 bus trace matches expected transaction program")
        return 0
    print("FAIL: APB3 bus trace did not match expected program", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
