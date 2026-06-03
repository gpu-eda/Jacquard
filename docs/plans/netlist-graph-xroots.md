# Plan: `xroots` — backward X-source frontier query

Issue: [#98](https://github.com/gpu-eda/Jacquard/issues/98). Builds on the
`netlist-graph` cone/driver fixes in [#101](https://github.com/gpu-eda/Jacquard/pull/101)
(PR1), which is why this branch is stacked on `fix/netlist-graph-cone-drivers`.

## Problem

When a signal reads X under `jacquard cosim --xprop`, finding *why* is a
manual trace→guess→re-run loop: the VCD can only report wires you already
chose to trace, so you must half-know the answer to ask the question. But the
information is **static**: an X originates at a known X-source (unreset DFF Q,
SRAM read port, undriven primary input) and propagates forward. So *"what
makes S read X?"* = *"which X-sources lie in S's backward cone?"* — a pure
netlist query.

## Design decisions (confirmed)

1. **Driven-input set comes from jacquard, not re-derived in Python.** The
   authoritative driven set (clock/reset/constant/peripheral pins + GPIO→port
   mapping) is computed in Rust during cosim setup. A new `jacquard xsources`
   subcommand dumps the X-source set (including the undriven-input complement)
   as JSON; `netlist-graph xroots` consumes it. No drift, single source of
   truth.
2. **Dominators deferred.** v1 ships the frontier query + classification +
   `--emit-trace`. Dominators (X-sources every path passes through) need a
   careful formulation over the DFF-feedback cyclic graph and land as a
   scoped follow-up once the frontier query proves useful.

## Part A — `jacquard xsources` (Rust)

A new clap subcommand alongside `Sim` / `Cosim` in `src/bin/jacquard.rs`.

```
jacquard xsources <netlist> --config <sim_config.json> -o xsources.json
```

Reuses the existing cosim setup path to build the `AIG` + `NetlistDB` and the
driven-input set, then:

- **DFF-Q / SRAM-read X-sources** from `AIG::compute_x_sources()`
  (`src/aig.rs:3277`) — already enumerates these as AIG pins.
- **Undriven primary inputs** = primary-input bits *not* in the cosim driven
  set (the same complement `cosim --xprop` treats as X; see
  `cosim-xprop.md` X-source taxonomy rows 3–4).
- **Name resolution**: map each X-source AIG pin → hierarchical net name via
  the existing `aigpin → (cell_id, cell_type, output_pin_name)` map
  (`src/aig.rs:265`) and `NetlistDB` pin/net names. `WordSymbolMap`
  (`src/flatten.rs:138`) is the precedent for this AIG→name translation.

**Output schema** (`schema_version: "1.0"`, additive-only):

```json
{
  "schema_version": "1.0",
  "netlist": "design.v",
  "x_sources": [
    {"net": "top.cpu.regs[7]", "kind": "unreset-dff", "cell": "..."},
    {"net": "top.sram.rd_data[3]", "kind": "sram-read", "cell": "..."},
    {"net": "ext_in[2]", "kind": "undriven-input"}
  ]
}
```

`kind` ∈ {`unreset-dff`, `sram-read`, `undriven-input`}. The net names are
emitted in the same conventions `--trace-signals` resolves, so they round-trip
into the Python tool and back into a confirming `--xprop` run.

> **Note on "unreset":** jacquard marks *all* DFF Qs as X at cycle 0; a DFF
> with a connected reset resolves once reset asserts. For a static backward
> query the useful classification is "is this DFF reset-connected?" — emitted
> as `unreset-dff` only when no reset/set pin is wired. This is a static
> over-approximation (documented in the command help).

## Part B — `netlist-graph xroots` (Python)

```
netlist-graph xroots <netlist> <signal> [--xsources xsources.json] [--emit-trace <file>] [-d DEPTH]
```

1. **Resolve** `<signal>` to a net (reuse `resolve_name`).
2. **Reverse-reachability** from the net through `find_drivers`, continuing
   **through** DFF data pins (the same data-path-through-registers walk as
   `logic_cone(through_regs=True)` restricted to `_DFF_DATA_PINS` — reuses
   PR1's corrected `_is_register`). Clock/reset pins are not followed.
3. **X-source set**:
   - With `--xsources`: use the manifest's authoritative net set +
     classification (covers `undriven-input`, which the netlist alone can't
     classify).
   - Without it: classify natively from the netlist — DFF-Q nets (cells
     `_is_register` matches) and SRAM read-port nets — and emit
     `undriven-input` as *unknown* (warn that `--xsources` is needed for the
     driven-set complement). Genuinely-undriven internal nets (PR1's
     `[undriven — X-source]` leaves) are reported as candidate roots.
4. **Frontier**: BFS outward; when a reached net is an X-source, record it and
   stop expanding past it (it is a root). Non-source nets keep expanding.
   Frontier = X-sources reachable without passing through another X-source.
5. **Report**: frontier X-sources, classified and grouped by kind, nearest
   first (BFS depth). `--emit-trace <file>` writes them as a `--trace-signals`
   list (one net per line, `# kind` comments) so a confirming `--xprop` run is
   one command.

### Reuse / new code

- Reuses: `resolve_name`, `find_drivers`, `_is_register`, `_DFF_DATA_PINS`,
  `_short_net`, the `out_driver`/`is_driven` machinery from PR1.
- New: `xroots` graph method (reverse-reachability + frontier intersection),
  `xroots` CLI command, manifest loader.

## Tests

- **Rust**: `xsources` unit test on a small netlist+config — assert DFF-Q,
  SRAM-read, and undriven-input nets appear with correct `kind`; reset-
  connected DFFs are *not* `unreset-dff`.
- **Python**: synthetic netlist with a known X-source behind two logic
  levels and a reset-defined path; assert the frontier finds the source, the
  classification matches the manifest, `--emit-trace` output round-trips, and
  the "no manifest" mode warns.
- **Integration**: `xsources` on `tests/timing_test/minimal_build` →
  `xroots --emit-trace` → feed back to `cosim --xprop --trace-signals` and
  confirm the surfaced wires carry X (smoke test, gated on Metal availability).

## Sequencing

1. Part A (`jacquard xsources`) + Rust tests.
2. Part B (`netlist-graph xroots`) + Python tests, consuming Part A's manifest.
3. Docs: a `docs/x-debugging.md` user guide (xsources → xroots → confirming
   --xprop run) + CHANGELOG entries + a one-line pointer in CLAUDE.md's
   debugging-tools section.

## Out of scope (follow-ups)

- Dominator analysis (decision 2).
- Wire-bundle reconstruction of multi-bit X-source buses (tracked separately).
