# Handoff — derived (on-die divided) clock support (#185)

**Branch:** `feat/derived-clock-divider` (off `main` @ `346b0169`).
**Issue:** gpu-eda/Jacquard#185. **Plan comments:** posted on the issue.

## Goal

Let cosim handle an on-die DFF clock divider (a flop clocked by one clock whose
`Q` is a derived clock for other flops). Currently `trace_clock_pin` panics
(`aig.rs:2676`) because the divider DFF is a multi-input cell on the clock path.
Chosen approach: **Direction 2** — config declares the derived clock; the AIG
cuts the divider and the scheduler drives the net as a commensurable ÷N domain
(mirrors STA `create_generated_clock`).

## Done (committed on the branch)

1. `2968f96d` — **minimal ÷2 repro fixture** `tests/derived_clock/`: a toggle-DFF
   divider feeding a counter. Reproduces the exact panic
   (`multi-input cell driving _40_:Q ... aig.rs:2676`) in a tiny design. Fast
   gate, independent of the 100k-cell C64 netlist.
2. `27d45749` — **config schema foundation.** `ClockConfig.gpio`/`period_ps` now
   `Option`; derived clock declared by `{ net, derived_from, divide_by }`
   (+ `is_derived()`). Backward-compatible. Scheduler-timing matching handles the
   Option; derived entries skipped with a "not yet implemented" warn. **No
   behavior change** for existing configs (multi_mem verified unchanged).

## Validated design (from reading the code — this is the load-bearing part)

- **The scheduler already supports a commensurable ÷2.** `MultiClockScheduler::new`
  (`mod.rs:1482`) builds the schedule from gcd/lcm of half-periods.
- **The scheduler drives clock domains by their FLAG bits, and `clock_input_pos`
  is optional** (`build_edge_ops`, `mod.rs:1584-1621`, `if let Some(pos)`). This
  is the unlock: a derived clock needs **no** primary-input value position — the
  cut's `InputClockFlag` posedge/negedge flags are enough.
- **The tracer already mints an `InputClockFlag` at primary inputs / undriven
  nets** (`aig.rs:872-895`). The cut reuses that path, triggered for a
  config-declared net.

## Remaining work (exact sites)

### A. Thread derived-clock net names into AIG build
- `DesignArgs` (`src/sim/setup.rs:23`): add `pub derived_clock_nets: Vec<String>`.
- `build_netlist_and_aig` (`setup.rs:109`): add param `derived_clock_nets: &[String]`,
  pass to the constructor at `setup.rs:211`.
- Callers to update: `load_design` (`setup.rs:382`, pass `args.derived_clock_nets`),
  `jacquard.rs:2090` (xsources, `&[]`), `x_sources.rs:330,416` (tests, `&[]`).
- `from_netlistdb_with_cells_and_descriptor` (`aig.rs:2531`) + `from_netlistdb_impl`
  (`aig.rs:2563`): add the param; the other public constructors
  (`from_netlistdb`, `_with_cells`, `_with_pdk`) pass `&[]`.
- In `from_netlistdb_impl`, before the clock-tracing loop, resolve the net names
  to netlistdb net ids (via `netname2id` / `parse_signal_name`) into a new AIG
  field `derived_clock_netids: HashSet<usize>` (add to the struct + `Default`).

### B. The cut, in `trace_clock_pin` (`aig.rs:768`)
At the point it finds the net's driver pin (`aig.rs:868`, `if let Some(dp) = driver_pin`):
if `self.derived_clock_netids.contains(&netid)`, **do not** follow to `dp`
(the divider FF's `Q`). Instead terminate and mint an `InputClockFlag` keyed on
`dp` (same logic as the undriven-net branch, `aig.rs:875-895`), so all downstream
flops on the net share one clock domain. The divider FF stays in the AIG as
(now dead-for-clock) logic that still computes the net's observable value.

### C. Scheduler binding (`mod.rs` clock-timings block ~`2364`)
The cut makes the divider-`Q` pin a clock domain in `gpio_map.clock_domains`
(`clock_gpio: None`, `name` = the Q pin, flags populated, `clock_input_pos: None`
— all fine). Replace the current "skip derived" branch with real matching:
- For a `clk_cfg.is_derived()` entry, resolve `clk_cfg.net` → net id → its driver
  pin → find the `clock_domains` entry whose `clock_pinid` == that pin.
- Compute `half_period_ps = parent_period(clk_cfg.derived_from) × divide_by / 2`.
- Emit a `ClockDomainTiming { half_period_ps, phase_offset_ps, domain_index }`.
The gcd/lcm scheduler then interleaves it as a ÷N domain; `build_edge_ops` drives
its flag bits.

### D. Wire config → DesignArgs (`jacquard.rs` cmd_cosim ~`2277`)
Collect `config.effective_clocks().iter().filter(|c| c.is_derived()).filter_map(|c| c.net.clone())`
into `design_args.derived_clock_nets`. (cmd_sim can pass `Vec::new()`.)

### E. Fixture + test
Switch `tests/derived_clock/sim_config.json` to the derived form (below), assert
the counter increments at half rate (a `check.py` + golden), wire into the
`cosim_cpu_check.sh` `all` scope. Target config:

```jsonc
{
  "reset_gpio": 1, "reset_active_high": false, "reset_cycles": 4,
  "num_cycles": 60, "clock_gpio": 0,
  "clocks": [
    { "gpio": 0, "period_ps": 15625, "name": "clk" },
    { "net": "clkdiv", "derived_from": "clk", "divide_by": 2, "name": "clkdiv" }
  ],
  "port_mapping": { "inputs": {"0":"clk","1":"rst_n"},
                    "outputs": {"2":"clkdiv","3":"count[0]", ...} }
}
```

## Correctness note

The real divider holds `clkdiv` during reset (`RN`); a scheduler-driven domain
would toggle from t=0. Post-reset behaviour is identical. Add a check that the
derived domain's edges line up with the divider after reset (or gate the derived
domain on reset). Also confirm the divider FF's observable `clkdiv` value still
matches the scheduler's ÷2 phase.

## Risk

The cut edits `trace_clock_pin`, which gates **every** design. Run the full
`cosim_cpu_check.sh all` golden suite (and Metal) after — a mis-keyed
`InputClockFlag` or a cut firing on the wrong net would surface there.
