// SPDX-License-Identifier: Apache-2.0

//! Liberty library loading — shared by the `liberty-to-cellir` CLI and by
//! callers that drive the converter as a library (e.g. Jacquard's `build.rs`
//! build-time descriptor generation, ADR 0019 D7).
//!
//! ## Split-library handling
//!
//! Some PDKs (notably GF180MCU) ship a top-level `.lib` carrying only
//! library-level attributes, with each *cell* in its own per-cell `.lib`
//! under a sibling `cells/` tree. When the named `.lib` contains no `cell`
//! groups, [`load_library`] discovers and merges the per-cell `.lib` files
//! that share the same corner suffix, so a real multi-cell descriptor results.
//!
//! File discovery is deterministic (`files.sort()` before merge) so the
//! resulting [`cell_model_ir::CellModelIr`] — and its serialized JSON — is
//! byte-identical across repeated runs (ADR 0019 D7 determinism requirement).

use std::path::{Path, PathBuf};

use liberty_parse::LibertyGroup;

use crate::convert::convert_library;
use cell_model_ir::CellModelIr;

/// Load a Liberty library and convert it to a cell-model-IR descriptor in one
/// call — the library entry point mirroring the CLI's generate path.
///
/// `prefixes` is the D8 selection prefix set; pass an empty `Vec` to derive it
/// from the common cell-name prefix.
///
/// Deterministic: the same inputs always yield a byte-identical descriptor
/// (see module docs).
pub fn generate_descriptor(input: &Path, prefixes: Vec<String>) -> Result<CellModelIr, String> {
    let lib = load_library(input)?;
    Ok(convert_library(&lib, prefixes).ir)
}

/// Load a Liberty library, transparently merging a split per-cell library if
/// the named file carries no `cell` groups.
pub fn load_library(input: &Path) -> Result<LibertyGroup, String> {
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
