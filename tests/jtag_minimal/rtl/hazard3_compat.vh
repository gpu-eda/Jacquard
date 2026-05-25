// SPDX-License-Identifier: Apache-2.0
// Minimal compat header for vendored Hazard3 debug HDL — pulled here
// (rather than vendoring `deps/hazard3/hdl/hazard3_config.vh` in
// full) because the only macros the debug subtree actually
// references are flop-attribute hints that are safe to no-op for a
// gate-level sim/synth flow.

`ifndef _HAZARD3_COMPAT_VH
`define _HAZARD3_COMPAT_VH

// Suppresses synthesis attributes that would otherwise pin specific
// flops against retiming. Yosys's default behaviour on a no-op define
// is equivalent — we want every flop the design declares to land in
// the netlist for the AIG to walk.
`define HAZARD3_REG_KEEP_ATTRIBUTE

`endif
