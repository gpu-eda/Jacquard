# IHP SG13G2 combinational sim — zero-per-PDK-Rust proof (ADR 0019 D7a, C3a)

This fixture proves that IHP's open **SG13G2** PDK was added to Jacquard as a
new built-in PDK **purely by vendoring its submodule + embedding a generated
cell-model-IR descriptor** — with **no IHP-specific Rust anywhere**. IHP
combinational `sg13g2_*` cells simulate from the descriptor, auto-selected by
the `sg13g2_` cell-name prefix through the same PDK-neutral splice path
(`AIG::try_descriptor_comb`) that GF180/SKY130/AIGPDK use.

## Files

- `ihp_comb_top.v` — gate-level design. A cone of 12 distinct **combinational**
  `sg13g2_*` cell types (inv, buf, nand2, nor2, and2, or2, xor2, xnor2, a21oi,
  a22oi, o21ai, mux2), including multi-input cells (A1/A2/B1[/B2], A0/A1/S). The
  primary inputs are clocked through a bank of **AIGPDK** DFFs — a pure timing
  harness so GEM's clock-edge-driven simulator can sweep many input vectors past
  the combinational cone. **No IHP flops** (sequential descriptor-driving for
  non-GF180 PDKs is the deferred C3.3d work).
- `sg13g2_pins.v` — port-only stubs supplying the leaf-cell **pin directions**
  via `--cell-library` (ADR 0010 RuntimeCellLibrary) — an orthogonal *data*
  layer, not Rust. (A future descriptor-backed leaf-pin provider, C3.3d, would
  let the descriptor's own L1 directions replace this stub.)
- `ihp_comb_stim.vcd` — 16-vector clocked stimulus.
- `check.py` — independent truth-table validator (see below).

## Run (Metal; use `--features cuda`/`hip` elsewhere)

```bash
printf 'ra\nrb\nrc\nrd\nre\nrf\nrsel\n' > /tmp/ihp_trace.txt

cargo run -r --features metal --bin jacquard -- sim \
    tests/ihp_comb/ihp_comb_top.v \
    tests/ihp_comb/ihp_comb_stim.vcd \
    /tmp/ihp_comb_out.vcd 1 \
    --cell-library tests/ihp_comb/sg13g2_pins.v \
    --trace-signals /tmp/ihp_trace.txt \
    --check-with-cpu

python3 tests/ihp_comb/check.py /tmp/ihp_comb_out.vcd
```

## What passing proves

1. **`Auto-selected bundled cell-model-IR descriptor 'sg13g2'`** — the IHP
   descriptor is picked up by prefix, with zero IHP Rust.
2. **`sanity test passed!`** (`--check-with-cpu`) — the GPU kernel agrees with an
   independent CPU reference on the IHP-descriptor-spliced AIG, every cycle.
3. **`check.py` → PASS (176 checks, 0 mismatches)** — the descriptor's
   combinational logic is not merely self-consistent but *correct* versus
   hand-derived truth tables for every cell type. (The generation-time
   Liberty-vs-`.v` cross-check does not run for IHP — its `.v` is one flat-module
   file, a C4 indexer limitation — so this VCD check is the logic oracle.)

## Deferred

- **Sequential IHP** (the 14 `ff` cells) awaits **C3.3d**, which makes sequential
  descriptor-driven for non-GF180 PDKs. The IHP descriptor already carries their
  L3 — full IHP support then needs **zero additional IHP Rust**.
- **C4:** flat-`.v` cross-check indexing; the one set-dominant flop
  `sg13g2_sdfbbp_1` (`clear_preset_var1=H`) needs the schema `clear_preset`
  tie-break field.
