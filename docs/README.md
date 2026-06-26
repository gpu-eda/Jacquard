# Jacquard Documentation

Welcome to the documentation for Jacquard, a GPU-accelerated RTL logic simulator.

Use the sidebar to navigate between topics, or start with the [Getting Started](usage.md) guide.

## Documents

### Project Scope & Planning

Start here if you're considering a feature contribution or want to understand Jacquard's overall direction.

- **[Project Scope & Guarantees](project-scope.md)**: Top-level contract — what Jacquard is for, what it isn't, licensing and architecture constraints, stability tiers.
- **[Why Jacquard](why-jacquard.md)**: Honest positioning vs. STA tools and event-driven simulators; what's unique, what isn't, and what output interface would let users extract the value.
- **[Timing Correctness](timing-correctness.md)**: Scoped requirements for timing accuracy, validation, and the forthcoming timing IR.
- **[Timing Model Extensions](timing-model-extensions.md)**: Pre-spike design notes for δ(T) dynamic delay, clock-tree skew, and wire delay at scale. Formalised in ADR 0007.
- **[Post-Phase-0 Roadmap](plans/post-phase-0-roadmap.md)**: Sequencing of Phase 1+ work covering structured timing output (ADR 0008) and timing model fidelity (ADR 0007). (OpenTimer integration was originally Phase 1's centrepiece; ADR 0003 was Superseded by the spike — OpenSTA out of process is now the sole STA path per ADR 0001.)
- **[Architecture Decision Records](adr/README.md)**: Design decisions and their rationale (numbered, per-decision). See the index for status and how the ADRs relate.
- **[Implementation Plans](plans/README.md)**: Phased implementation plans with entry and exit criteria. See the index for status and reading order.
- **[Spikes](spikes/)**: Time-boxed experiments and their outcomes.

### Core Documentation

- **[Simulation Architecture](simulation-architecture.md)**: Detailed explanation of Jacquard's internal architecture
  - Pipeline stages (NetlistDB → AIG → StagedAIG → Partitions → FlattenedScript → GPU)
  - Data structures and representations
  - VCD input/output format requirements
  - Assertion and display support infrastructure
  - Performance characteristics
  - Known issues and limitations

- **[Timing Simulation](timing-simulation.md)**: CPU-based timing simulation with Liberty/SDF delays
- **[Timing Violations](timing-violations.md)**: GPU-side setup/hold violation detection

### Troubleshooting Guides

- **[Troubleshooting VCD](troubleshooting-vcd.md)**: Debugging VCD input issues
  - VCD hierarchy requirements
  - Signal naming and matching
  - Solutions for flat VCD generation
  - Diagnostic checklist
  - Working examples

## Quick Reference

### VCD Input Requirements (Critical!)

Jacquard expects VCD signals at **absolute top-level** (no module hierarchy):

```verilog
// ✓ Correct testbench
initial begin
    $dumpfile("output.vcd");
    $dumpvars(1, clk, reset, din, dout);  // Depth 1, explicit signals
end

// ✗ Incorrect testbench
initial begin
    $dumpfile("output.vcd");
    $dumpvars(0, testbench);  // Dumps entire hierarchy
end
```

### Debug Commands

```bash
# Enable debug logging
RUST_LOG=debug cargo run -r --features metal --bin jacquard -- sim <args>

# Verify with CPU simulation
cargo run -r --features metal --bin jacquard -- sim <args> --check-with-cpu

# Check VCD structure
grep '\$scope\|\$var' input.vcd | head -20
```

### Cosim (reactive peripherals)

`jacquard cosim` runs GPU-resident peripheral models (SPI flash, UART, Wishbone)
alongside the design so inputs can react to outputs cycle-by-cycle. It runs on
Metal, CUDA, and HIP (plus a CPU fallback).

```bash
# Drive the design from a JSON testbench config; write an output VCD
cargo run -r --features metal --bin jacquard -- cosim \
    design.v --config sim_config.json --output-vcd out.vcd
```

| Flag | Purpose |
|------|---------|
| `--config <json>` | Testbench config: clock(s), reset, peripherals (required) |
| `--output-vcd <path>` | Output VCD (chip outputs + any traced nets) |
| `--trace-signals <path>` | Surface internal nets in the VCD ([Signal Tracing](signal-tracing.md)) |
| `--bus-trace-csv <path>` | Decode on-chip bus transactions ([Bus Tracing](bus-tracing.md)) |
| `--jtag-server <port>` / `--jtag-replay <path>` | Interactive / deterministic JTAG debug ([JTAG Debug](jtag-debug.md)) |
| `--xprop` | Selective X-propagation for uninitialised state |
| `--max-clock-edges <n>` | Limit simulation length (1 cycle = 2 edges) |

### Key Statistics

When running Jacquard, look for these diagnostic outputs:

```
netlist has X pins, Y aig pins, Z and gates        # AIG complexity
current: N endpoints, try M parts                  # Partition count
Built script for B blocks, reg/io state size S     # Final script
WARN (GATESIM_VCDI_MISSING_PI) ...                 # VCD issues!
```

## Investigation Methodology

This documentation was created through systematic investigation of Jacquard's behavior:

1. **Source Code Analysis**: Examined `src/aig.rs`, `src/flatten.rs`, `src/staging.rs`
2. **Debug Tracing**: Used `RUST_LOG=debug` to capture internal state
3. **Test Case Development**: Created minimal reproducible examples
4. **Comparative Testing**: Compared Jacquard vs iverilog outputs
5. **Third-Party Validation**: Tested with real-world examples (sva-playground)

## Known Issues

Tracked live on GitHub — see the
[open issues](https://github.com/gpu-eda/Jacquard/issues) and the
[`priority:high`](https://github.com/gpu-eda/Jacquard/labels/priority%3Ahigh)
label. The long-standing ones:

- **VCD hierarchy mismatch** — Jacquard expects a flat top-level VCD; most
  testbenches emit hierarchical ones. Workaround: `--input-vcd-scope` (see
  [Troubleshooting VCD](troubleshooting-vcd.md)). Tracking:
  [#142](https://github.com/gpu-eda/Jacquard/issues/142).
- **Complex FSM simulation** — some FSM designs (e.g. `safe.v`) don't simulate
  correctly; under investigation. Tracking:
  [#143](https://github.com/gpu-eda/Jacquard/issues/143).
- **Format-string preservation** — Yosys may drop `gem_format` attributes, so
  `$display` messages show placeholders. This is an upstream Yosys limitation;
  the workaround is to extract format strings from the pre-synthesis JSON.

## Contributing

When adding documentation:

1. **Be specific**: Include actual commands, file paths, code snippets
2. **Show examples**: Both working and non-working cases
3. **Link related docs**: Cross-reference other documentation files
4. **Date updates**: Update version and date at bottom of documents
5. **Test instructions**: Verify all commands actually work

## Future Documentation Needs

Dedicated guides not yet written (coverage today is scattered across ADRs and
reference docs):

- [ ] Performance tuning guide (choosing `NUM_BLOCKS`, `--level-split`)
- [ ] SRAM modeling & synthesis (synthesis flow + preload + observability in one place)
- [ ] Multi-clock domain user guide (config examples; cf. [#87](https://github.com/gpu-eda/Jacquard/issues/87) for test coverage)
- [ ] GPU kernel optimization internals (profiling, backend-specific tuning)

Now covered: custom cell libraries → [Adding a New PDK](adding-a-pdk.md) +
ADR 0010/0011; VCD scope behaviour → [Troubleshooting VCD](troubleshooting-vcd.md).

## Related Resources

- **Main README**: `../README.md` - Project overview and quick start
- **CLAUDE.md**: `../CLAUDE.md` - Development guidelines and architecture overview
- **Test Suite**: `../tests/` - Examples and regression tests
- **Third-Party Tests**: `../tests/regression/third_party/` - Real-world examples with attribution

---

**Last Updated**: 2026-06-26
**Maintained By**: gpu-eda community
