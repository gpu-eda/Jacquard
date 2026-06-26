// SPDX-License-Identifier: Apache-2.0

//! `liberty-to-cellir` CLI (ADR 0019 D6).
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
use liberty_parse::LibertyGroup;

use liberty_to_cellir::convert::{convert_library, ConvertNote};
use liberty_to_cellir::crosscheck::{check_cell, CellCheck, ModelIndex};
use liberty_to_cellir::sequential::SeqNote;

#[derive(ClapParser, Debug)]
#[command(
    name = "liberty-to-cellir",
    about = "Generate a cell-model-IR descriptor from a Liberty library (ADR 0019 D6)."
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
         json_bytes={json_bytes}",
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

/// Load a Liberty library, transparently merging a split per-cell library if
/// the named file carries no `cell` groups.
fn load_library(input: &Path) -> Result<LibertyGroup, String> {
    let content =
        std::fs::read_to_string(input).map_err(|e| format!("reading {}: {e}", input.display()))?;
    let mut lib =
        liberty_parse::parse(&content).map_err(|e| format!("parsing {}: {e}", input.display()))?;

    if lib.groups_of_type("cell").next().is_some() {
        return Ok(lib);
    }

    // No cells in the top-level lib — try the split per-cell layout.
    let merged = discover_split_cells(input, &lib)?;
    if merged.is_empty() {
        eprintln!(
            "warning: {} contains no `cell` groups and no per-cell sibling \
             .lib files were found; descriptor will be empty",
            input.display()
        );
        return Ok(lib);
    }
    eprintln!(
        "split-library: merged {} per-cell .lib files alongside {}",
        merged.len(),
        input.display()
    );
    lib.groups.extend(merged);
    Ok(lib)
}

/// Find per-cell `.lib` files that share the corner suffix of `input` and
/// return their `cell` groups. Looks under a sibling `cells/` directory
/// (the GF180/SKY130 layout) relative to the liberty dir.
fn discover_split_cells(input: &Path, top: &LibertyGroup) -> Result<Vec<LibertyGroup>, String> {
    let stem = input
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("input has no file stem")?;
    let lib_name = top.first_name().unwrap_or(stem);

    // Corner suffix = the part of the stem after the library name, e.g.
    // `gf180mcu_fd_sc_mcu7t5v0__tt_025C_5v00` minus the lib name leaves
    // `__tt_025C_5v00`.
    let corner_suffix = stem
        .strip_prefix(lib_name)
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            // Fallback: last "__<...>" run.
            stem.rfind("__")
                .map(|i| stem[i..].to_string())
                .unwrap_or_default()
        });

    // Candidate cells directories: sibling `cells/` of the liberty dir, or
    // the PDK root's `cells/`.
    let lib_dir = input.parent().unwrap_or_else(|| Path::new("."));
    let mut cells_dirs = Vec::new();
    cells_dirs.push(lib_dir.join("cells")); // liberty/cells (unlikely)
    if let Some(pdk_root) = lib_dir.parent() {
        cells_dirs.push(pdk_root.join("cells")); // <pdk>/cells (GF180 layout)
    }

    let want_suffix = format!("{corner_suffix}.lib");
    let mut files = Vec::new();
    for dir in &cells_dirs {
        collect_matching_libs(dir, &want_suffix, &mut files);
    }
    files.sort();

    let mut groups = Vec::new();
    let mut parse_errors = 0usize;
    for path in files {
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        // Per-cell `.lib` files in the GF180/SKY130 split layout are bare
        // `cell(...) { .. }` groups (often preceded by a license comment)
        // with no enclosing `library(..)` — they are meant to be `include`d
        // inside the top-level library. The `library`-rooted parser rejects
        // them as-is, so try a bare parse first and fall back to wrapping the
        // content in a synthetic library.
        let parsed = liberty_parse::parse(&src)
            .or_else(|_| liberty_parse::parse(&format!("library(_split) {{\n{src}\n}}\n")));
        match parsed {
            Ok(cell_lib) => {
                for g in cell_lib.groups_of_type("cell") {
                    groups.push(g.clone());
                }
            }
            Err(_) => parse_errors += 1,
        }
    }
    if parse_errors > 0 {
        eprintln!("split-library: {parse_errors} per-cell .lib files failed to parse");
    }
    Ok(groups)
}

fn collect_matching_libs(dir: &Path, want_suffix: &str, out: &mut Vec<PathBuf>) {
    if !dir.is_dir() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_matching_libs(&path, want_suffix, out);
        } else if path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.ends_with(want_suffix))
            .unwrap_or(false)
        {
            out.push(path);
        }
    }
}
