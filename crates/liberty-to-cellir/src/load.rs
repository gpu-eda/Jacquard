// SPDX-License-Identifier: Apache-2.0

//! Liberty library loading — shared by the `liberty-to-cellir` CLI and by
//! callers that drive the converter as a library (e.g. Jacquard's `build.rs`
//! build-time descriptor generation, Decision 0019 D7).
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
//! byte-identical across repeated runs (Decision 0019 D7 determinism requirement).

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
///
/// A `.lib.json` input (SKY130's only Liberty form — JSON-serialized, never
/// `.lib` text) is routed to [`load_json_library`], which assembles a
/// per-corner library from the per-corner header plus every per-cell-per-corner
/// cell file. Plain `.lib` text keeps the existing GF180/text path.
pub fn load_library(input: &Path) -> Result<LibertyGroup, String> {
    if is_lib_json(input) {
        return load_json_library(input);
    }

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

/// Whether `input` names a `.lib.json` file (SKY130's JSON-serialized Liberty).
fn is_lib_json(input: &Path) -> bool {
    input
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.ends_with(".lib.json"))
        .unwrap_or(false)
}

/// Assemble a per-corner Liberty library from SKY130's `.lib.json` layout.
///
/// `header` is a per-corner library header
/// (`timing/sky130_fd_sc_hd__<corner>.lib.json`) carrying only library-level
/// attributes (`operating_conditions`, `default_operating_conditions`,
/// `nom_*`, `lu_table_template`, ...). Each *cell* lives in its own
/// per-cell-per-corner file
/// (`cells/<group>/sky130_fd_sc_hd__<cell>__<corner>.lib.json`) whose
/// top-level object *is* the body of one `cell (...)` group — the cell name is
/// the filename minus the `__<corner>.lib.json` suffix.
///
/// This reads the header as the `library` group, then discovers and appends
/// every cell file matching the header's corner suffix (deterministically
/// sorted), each decoded into a named `cell (...)` group. The resulting tree
/// is identical in shape to a text-parsed monolithic Liberty library, so the
/// converter — including the corner-from-Liberty-PVT logic that reads the
/// header's `operating_conditions` — consumes it unchanged.
pub fn load_json_library(header: &Path) -> Result<LibertyGroup, String> {
    let header_name = header
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("header path has no file name")?;
    let lib_name = header_name
        .strip_suffix(".lib.json")
        .ok_or("header is not a .lib.json file")?
        .to_string();

    let content = std::fs::read_to_string(header)
        .map_err(|e| format!("reading {}: {e}", header.display()))?;
    let mut lib = liberty_parse::json::parse_group(&content, "library", vec![lib_name.clone()])
        .map_err(|e| format!("parsing {}: {e}", header.display()))?;

    // SKY130's `.lib.json` headers omit the library-level `time_unit`. The
    // Liberty standard default is `1ns` (and SKY130's native unit is `1ns`),
    // but the converter's `ps_per_time_unit(None)` falls back to `1ps`, which
    // would mis-scale every L4 delay by 1000x. Restore the dropped default so
    // L4 timing is emitted in true picoseconds (a dff setup of `0.0508` ns
    // becomes 50.8 ps, not 0.05 ps).
    if lib.attr("time_unit").is_none() {
        lib.attributes.push(liberty_parse::Attribute {
            name: "time_unit".to_string(),
            values: vec![liberty_parse::Value::String("1ns".to_string())],
        });
    }

    // Corner suffix = the trailing `__<corner>` run of the library name, e.g.
    // `sky130_fd_sc_hd__tt_025C_1v80` ⇒ `__tt_025C_1v80`.
    let corner_suffix = lib_name
        .rfind("__")
        .map(|i| lib_name[i..].to_string())
        .ok_or_else(|| format!("cannot derive corner from header name {lib_name}"))?;
    let want_suffix = format!("{corner_suffix}.lib.json");

    // SKY130 layout: header is under `<pdk>/timing/`, cells under `<pdk>/cells/`.
    let pdk_root = header
        .parent()
        .and_then(|p| p.parent())
        .ok_or("header has no PDK-root ancestor")?;
    let cells_dir = pdk_root.join("cells");

    let mut files = Vec::new();
    collect_matching_json_cells(&cells_dir, &want_suffix, &mut files);
    files.sort();

    let mut cells = Vec::new();
    let mut parse_errors = 0usize;
    for path in &files {
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        // Cell name = filename minus the `__<corner>.lib.json` suffix.
        let Some(cell_name) = file_name.strip_suffix(&want_suffix) else {
            continue;
        };
        let Ok(src) = std::fs::read_to_string(path) else {
            parse_errors += 1;
            continue;
        };
        match liberty_parse::json::parse_group(&src, "cell", vec![cell_name.to_string()]) {
            Ok(cell) => cells.push(cell),
            Err(_) => parse_errors += 1,
        }
    }

    if parse_errors > 0 {
        eprintln!("sky130 .lib.json: {parse_errors} per-cell files failed to parse");
    }
    if cells.is_empty() {
        eprintln!(
            "warning: no per-cell .lib.json files matching `*{want_suffix}` found under {}; \
             descriptor will be empty",
            cells_dir.display()
        );
    } else {
        eprintln!(
            "sky130 .lib.json: assembled {} cells (corner {corner_suffix}) alongside {}",
            cells.len(),
            header.display()
        );
    }
    lib.groups.extend(cells);
    Ok(lib)
}

/// Recursively collect per-cell `.lib.json` files ending with `want_suffix`,
/// skipping the `.ccsnoise.lib.json` CCS-noise variants (which carry no cell
/// logic/timing the converter consumes and would duplicate the base cell).
fn collect_matching_json_cells(dir: &Path, want_suffix: &str, out: &mut Vec<PathBuf>) {
    if !dir.is_dir() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_matching_json_cells(&path, want_suffix, out);
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.ends_with(".ccsnoise.lib.json") {
            continue;
        }
        if name.ends_with(want_suffix) {
            out.push(path);
        }
    }
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
