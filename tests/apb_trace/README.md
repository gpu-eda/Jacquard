# APB3 bus-transaction trace test

Validates Jacquard's config-driven bus-transaction tracer (ADR 0013) end
to end on a synthesized design.

## Design (`apb_trace.v`)

A self-contained APB3 system: an APB master FSM issues a fixed program of
four transfers to an always-ready 2-word register-file slave, then idles.

| # | dir | addr | data |
|---|-----|------|------|
| 0 | WR  | 0x00 | 0xCAFEBABE |
| 1 | WR  | 0x04 | 0x12345678 |
| 2 | RD  | 0x00 | 0xCAFEBABE |
| 3 | RD  | 0x04 | 0x12345678 |

The APB master outputs and the slave's `PRDATA` are `(* keep *)`
registers so their net names survive synthesis as DFF Q outputs — that
is what the tracer resolves the GPU capture against. `PREADY` is tied
high and `PSLVERR` low (both fold to constants; the tracer treats an
unresolved `PREADY` as always-ready and unresolved `PSLVERR` as 0). The
`trace_tap` output is a reduction over every traced signal, present only
to stop synthesis pruning the otherwise output-less design.

## Build + run

```bash
# Synthesize to AIGPDK (committed as apb_trace_synth.gv)
yosys -s tests/apb_trace/synth.tcl   # run from tests/apb_trace/

# Cosim with bus tracing → CSV
cargo run -r --features metal --bin jacquard -- cosim \
    tests/apb_trace/apb_trace_synth.gv \
    --config tests/apb_trace/sim_config.json \
    --top-module apb_trace \
    --max-clock-edges 200 \
    --bus-trace-csv apb_trace.csv

# Pass criterion: the four transactions above, by content
python3 tests/apb_trace/check.py apb_trace.csv
```

The CI step `Run APB3 bus-trace cosim (ADR 0013)` runs exactly this.

## Notes

- Address bits other than `paddr[2]` and data bits constant across the
  program are constant-folded in synthesis; the tracer reconstructs the
  full values correctly because the dropped bits are genuinely 0. The
  startup log line (`addr 1/8 bits`) reflects this — it is expected.
- APB3 only for now. AHB-Lite / AHB5 are the planned next phase; see
  `docs/plans/bus-transaction-tracing.md`.
