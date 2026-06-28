// SPDX-License-Identifier: Apache-2.0

//! Build-time-generated cell-model-IR descriptors embedded into the binary
//! (ADR 0019 D7).
//!
//! `build.rs` runs the `liberty-to-cellir` converter as a library over the
//! pinned vendored PDKs and writes each descriptor into `$OUT_DIR`; this module
//! `include_str!`s those artifacts so a released binary carries them with no
//! runtime `vendor/` dependency. The JSON is **not** checked in — it is a
//! deterministic function of the pinned submodules (D7), regenerated whenever
//! the vendored Liberty changes.
//!
//! In C3.1 this sits *alongside* the legacy per-PDK paths: the descriptors are
//! available and loadable, and a run opts in via `--bundled-descriptor <name>`
//! (with `--cell-descriptor <path>` remaining the explicit file override). The
//! C3.2 selection work turns this into prefix-driven auto-selection, and C3.3
//! makes it the default.

use std::path::Path;

use cell_model_ir::CellModelIr;

/// GF180MCU 7-track descriptor, generated from
/// `vendor/gf180mcu_fd_sc_mcu7t5v0` at the `tt_025C_5v00` corner.
pub const GF180MCU_7T_JSON: &str =
    include_str!(concat!(env!("OUT_DIR"), "/gf180mcu_7t.cellir.json"));

/// GF180MCU 9-track descriptor, generated from
/// `vendor/gf180mcu_fd_sc_mcu9t5v0` at the `tt_025C_5v00` corner.
pub const GF180MCU_9T_JSON: &str =
    include_str!(concat!(env!("OUT_DIR"), "/gf180mcu_9t.cellir.json"));

/// A bundled descriptor's stable selector name and embedded JSON, paired so
/// callers can list, match, or load them uniformly.
pub struct BundledDescriptor {
    /// Stable selector accepted by `--bundled-descriptor`.
    pub name: &'static str,
    /// The embedded JSON (the byte-for-byte build-time artifact).
    pub json: &'static str,
}

/// All descriptors embedded at build time. Order is stable.
pub const ALL: &[BundledDescriptor] = &[
    BundledDescriptor {
        name: "gf180mcu_7t",
        json: GF180MCU_7T_JSON,
    },
    BundledDescriptor {
        name: "gf180mcu_9t",
        json: GF180MCU_9T_JSON,
    },
];

/// Comma-separated list of valid `--bundled-descriptor` names, for error
/// messages and `--help`.
pub fn names() -> String {
    ALL.iter().map(|d| d.name).collect::<Vec<_>>().join(", ")
}

/// Look up a bundled descriptor's raw JSON by selector name.
pub fn json_by_name(name: &str) -> Option<&'static str> {
    ALL.iter().find(|d| d.name == name).map(|d| d.json)
}

/// Parse a bundled descriptor by selector name into a [`CellModelIr`].
///
/// Panics if the embedded JSON fails to parse — that would be a build-time
/// regression in the converter, not a user error.
pub fn load(name: &str) -> Option<CellModelIr> {
    json_by_name(name).map(|json| {
        CellModelIr::from_json(json)
            .unwrap_or_else(|e| panic!("embedded bundled descriptor '{name}' failed to parse: {e}"))
    })
}

/// Resolve the cell-model-IR descriptor a run should consume, applying the
/// ADR 0019 precedence: an explicit `--cell-descriptor <path>` file always
/// wins over a bundled `--bundled-descriptor <name>` selection; absent both,
/// `None` (the legacy per-PDK path).
///
/// Panics with an actionable message on a bad path or unknown bundled name.
pub fn resolve(explicit: Option<&Path>, bundled: Option<&str>) -> Option<CellModelIr> {
    if let Some(path) = explicit {
        let ir = CellModelIr::read_from(path)
            .unwrap_or_else(|e| panic!("loading --cell-descriptor {}: {e}", path.display()));
        return Some(ir);
    }
    if let Some(name) = bundled {
        let ir = load(name).unwrap_or_else(|| {
            panic!(
                "--bundled-descriptor '{name}' not found; available: {}",
                names()
            )
        });
        return Some(ir);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_descriptors_parse_and_are_nonempty() {
        for d in ALL {
            // Always: the embedded JSON must parse (a build-time regression
            // otherwise).
            let ir = load(d.name).expect("listed descriptor must load");

            // A build without the GF180 submodules embeds a valid *empty*
            // descriptor (see build.rs). The real-data assertions only apply
            // when the submodule was present at build time; the Unit Tests CI
            // job checks GF180 out, so they are live there.
            if ir.cells.is_empty() {
                eprintln!(
                    "skipping non-empty assertions for {}: empty descriptor \
                     (GF180 submodule absent at build time)",
                    d.name
                );
                continue;
            }
            assert!(
                ir.cells.len() > 100,
                "{} embedded descriptor has only {} cells",
                d.name,
                ir.cells.len()
            );
            assert!(
                !ir.library.prefixes.is_empty(),
                "{} descriptor should declare a D8 selection prefix",
                d.name
            );
        }
    }

    #[test]
    fn unknown_name_is_none() {
        assert!(load("nonexistent_pdk").is_none());
        assert!(json_by_name("nonexistent_pdk").is_none());
    }
}
