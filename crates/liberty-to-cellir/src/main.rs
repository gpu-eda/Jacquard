// SPDX-License-Identifier: Apache-2.0

//! `liberty-to-cellir` CLI (Decision 0019 D6).
//!
//! ```text
//! liberty-to-cellir <input.lib> [-o out.json] [--functional-v <dir>] [--prefix <p>]...
//! ```
//!
//! Generates a cell-model-IR descriptor from a Liberty library, optionally
//! cross-checking the Liberty-derived combinational logic against the PDK's
//! `functional.v` models (D6 "surface disagreement").
//!
//! ## Split-library handling
//!
//! Some PDKs (notably GF180MCU) ship a top-level `.lib` carrying only
//! library-level attributes, with each *cell* in its own per-cell `.lib`
//! under a sibling `cells/` tree. When the named `.lib` contains no `cell`
//! groups, this tool discovers and merges the per-cell `.lib` files that
//! share the same corner suffix, so a real multi-cell descriptor results.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser as ClapParser;

use liberty_to_cellir::convert::{convert_library, ConvertNote};
use liberty_to_cellir::crosscheck::{check_cell, check_cell_arcs, ArcCheck, CellCheck, ModelIndex};
use liberty_to_cellir::load::load_library;
use liberty_to_cellir::sequential::SeqNote;

#[derive(ClapParser, Debug)]
#[command(
    name = "liberty-to-cellir",
    about = "Generate a cell-model-IR descriptor from a Liberty library (Decision 0019 D6)."
)]
struct Cli {
    /// Input Liberty library (.lib). If it carries no `cell` groups, per-cell
    /// sibling `.lib` files of the same corner are discovered and merged.
    input: PathBuf,

    /// Output descriptor path (JSON). Defaults to `<input stem>.cellir.json`.
    #[arg(short = 'o', long = "out")]
    out: Option<PathBuf>,

    /// Directory tree of `functional.v` models for the D6 cross-check.
    #[arg(long = "functional-v")]
    functional_v: Option<PathBuf>,

    /// Cell-name selection prefix(es) (D8). Repeatable. If omitted, derived
    /// from the common cell-name prefix.
    #[arg(long = "prefix")]
    prefix: Vec<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<(), String> {
    let lib = load_library(&cli.input)?;

    let conv = convert_library(&lib, cli.prefix.clone());
    let seq_notes = conv.seq_notes;
    let ir = conv.ir;

    // --- summary counters ---
    let n_cells = ir.cells.len();
    let n_comb = ir.cells.iter().filter(|c| c.logic.is_some()).count();
    let n_l3 = ir.cells.iter().filter(|c| c.sequential.is_some()).count();
    let n_l4 = ir.cells.iter().filter(|c| c.timing.is_some()).count();
    let n_corners = ir.corners.len();

    // --- L3 sequential diagnostics, incl. the clear_preset precedence check ---
    let mut clear_preset_divergent = 0usize;
    let mut unparsed_controls = 0usize;
    let mut next_state_errors = 0usize;
    for note in &seq_notes {
        match note {
            SeqNote::ClearPresetNotResetDominant { cell, var1, var2 } => {
                clear_preset_divergent += 1;
                eprintln!(
                    "CLEAR_PRESET NOT RESET-DOMINANT: {cell} has clear_preset_var1={var1} \
                     clear_preset_var2={var2} (expected L/H). Jacquard's overlay is hardcoded \
                     reset-dominant — the schema needs a per-cell `clear_preset` field."
                );
            }
            SeqNote::UnparsedControl { cell, role, expr } => {
                unparsed_controls += 1;
                eprintln!("L3 unparsed {role} expression in {cell}: {expr:?}");
            }
            SeqNote::NextStateCompileError { cell, expr, error } => {
                next_state_errors += 1;
                eprintln!("L3 next_state compile error in {cell} ({expr:?}): {error}");
            }
        }
    }

    // Classify conversion notes. `sequential_outputs` (DFF/latch Q referencing
    // an internal state node) are EXPECTED, not errors — kept separate from
    // genuine parse failures so the summary line doesn't look alarming on a
    // well-formed library.
    let mut skipped_no_fn: BTreeMap<String, ()> = BTreeMap::new();
    let mut sequential_outputs = 0usize;
    let mut parse_errors = 0usize;
    for note in &conv.notes {
        match note {
            ConvertNote::SkippedNoFunction { cell } => {
                skipped_no_fn.insert(cell.clone(), ());
            }
            ConvertNote::SequentialOutput {
                cell,
                pin,
                operands,
            } => {
                sequential_outputs += 1;
                eprintln!("sequential output (not combinational at C1): {cell}.{pin} references {operands:?}");
            }
            ConvertNote::FunctionParseError { cell, pin, error } => {
                parse_errors += 1;
                eprintln!("function parse error: {cell}.{pin}: {error}");
            }
        }
    }
    for cell in skipped_no_fn.keys() {
        eprintln!("no Liberty function — skipped (needs .v fallback): {cell}");
    }

    // --- D6 cross-check ---
    let mut n_checked = 0usize;
    let mut n_capped = 0usize;
    let mut n_no_model = 0usize;
    let mut n_unevaluatable = 0usize;
    let mut mismatches = Vec::new();
    let mut n_arc_checked = 0usize;
    let mut n_arc_disagree = 0usize;
    let mut n_arc_no_specify = 0usize;
    if let Some(fv_dir) = &cli.functional_v {
        // The cross-check guards `.v` evaluation with `catch_unwind`; suppress
        // the default panic hook so caught panics don't spam stderr. The guard
        // restores the hook on *any* scope exit — including the early `?`
        // return below — so a real later panic isn't silently swallowed.
        struct HookGuard;
        impl Drop for HookGuard {
            fn drop(&mut self) {
                let _ = std::panic::take_hook();
            }
        }
        std::panic::set_hook(Box::new(|_| {}));
        let _hook_guard = HookGuard;
        let index = ModelIndex::scan(fv_dir)
            .map_err(|e| format!("scanning functional-v dir {}: {e}", fv_dir.display()))?;
        eprintln!(
            "cross-check: indexed {} .v models, {} UDPs from {}",
            index.models.len(),
            index.udps.len(),
            fv_dir.display()
        );
        // A 0-model index means the cross-check silently did nothing — the
        // requested `--functional-v` was a single flat-module `.v` file (e.g.
        // GF130's `GF013bcd_sc6_1p5_a0.v`, 2544 `module NAME(ports);` defs in
        // one file) or a directory with no per-cell `*.functional.v` models the
        // indexer recognises. Warn rather than report a misleading all-clear.
        // (Flat-module `.v` parsing is deferred to C4; this only surfaces it.)
        if index.models.is_empty() {
            eprintln!(
                "warning: cross-check indexed 0 .v models from {} — the D6 \
                 Liberty-vs-.v check did NOT run (the descriptor is unverified \
                 against the functional `.v`). The indexer expects a directory \
                 of per-cell `*.functional.v` models; a single flat-module `.v` \
                 is not yet supported (C4).",
                fv_dir.display()
            );
        }
        for cell in &ir.cells {
            match check_cell(cell, &index) {
                CellCheck::Checked {
                    cell: name,
                    assignments,
                    mismatches: ms,
                } => {
                    n_checked += 1;
                    if !ms.is_empty() {
                        eprintln!(
                            "CROSS-CHECK MISMATCH in {name} ({} of {assignments} assignments disagree):",
                            ms.len()
                        );
                        for m in &ms {
                            let vec: Vec<String> = m
                                .inputs
                                .iter()
                                .map(|(p, v)| format!("{p}={}", *v as u8))
                                .collect();
                            eprintln!(
                                "    pin {} @ [{}]: liberty={} functional_v={}",
                                m.pin,
                                vec.join(", "),
                                m.liberty as u8,
                                m.functional_v as u8
                            );
                        }
                        mismatches.extend(ms);
                    }
                }
                CellCheck::Capped { cell: name, inputs } => {
                    n_capped += 1;
                    eprintln!(
                        "cross-check CAPPED (not checked): {name} has {inputs} inputs (> {})",
                        liberty_to_cellir::crosscheck::MAX_EXHAUSTIVE_INPUTS
                    );
                }
                CellCheck::UnevaluatableModel { cell: name, reason } => {
                    n_unevaluatable += 1;
                    eprintln!("cross-check skipped (un-evaluatable model): {name}: {reason}");
                }
                CellCheck::NoModel => n_no_model += 1,
                CellCheck::NotComb => {}
            }
        }

        // --- L4 arc-set agreement (specify vs Liberty delay arcs) ---
        for cell in &ir.cells {
            match check_cell_arcs(cell, &index.specify) {
                ArcCheck::Checked {
                    cell: name,
                    missing,
                    extra,
                    ..
                } => {
                    n_arc_checked += 1;
                    if !missing.is_empty() || !extra.is_empty() {
                        n_arc_disagree += 1;
                        let fmt = |arcs: &[(String, String)]| {
                            arcs.iter()
                                .map(|(f, t)| format!("{f}=>{t}"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        };
                        eprintln!(
                            "ARC-SET MISMATCH in {name}: missing(liberty-only)=[{}] extra(.v-only)=[{}]",
                            fmt(&missing),
                            fmt(&extra)
                        );
                    }
                }
                ArcCheck::NoSpecify => n_arc_no_specify += 1,
                ArcCheck::NoTiming => {}
            }
        }
        // `_hook_guard` restores the default panic hook here on scope exit.
    }

    // --- write descriptor ---
    let out_path = cli.out.clone().unwrap_or_else(|| default_out(&cli.input));
    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).ok();
        }
    }
    ir.write_to(&out_path)
        .map_err(|e| format!("writing {}: {e}", out_path.display()))?;
    let json_bytes = std::fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);

    // --- summary line ---
    println!(
        "cells={n_cells} combinational={n_comb} l3_sequential={n_l3} l4_timing={n_l4} \
         corners={n_corners} cross_checked={n_checked} \
         cross_check_mismatches={} capped={n_capped} unevaluatable={n_unevaluatable} \
         no_model={n_no_model} skipped_no_function={} sequential_outputs={sequential_outputs} \
         function_parse_errors={parse_errors} clear_preset_divergent={clear_preset_divergent} \
         l3_unparsed_controls={unparsed_controls} l3_next_state_errors={next_state_errors} \
         arc_checked={n_arc_checked} arc_disagree={n_arc_disagree} \
         arc_no_specify={n_arc_no_specify} json_bytes={json_bytes}",
        mismatches.len(),
        skipped_no_fn.len(),
    );
    println!("wrote {}", out_path.display());

    Ok(())
}

fn default_out(input: &Path) -> PathBuf {
    let stem = input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("cellir");
    PathBuf::from(format!("{stem}.cellir.json"))
}
