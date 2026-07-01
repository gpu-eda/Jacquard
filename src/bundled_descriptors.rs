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

/// SKY130 high-density descriptor, generated from
/// `vendor/sky130_fd_sc_hd` at the `tt_025C_1v80` corner (assembled from the
/// vendor's `.lib.json` per-cell layout). Selected by the `sky130_fd_sc_hd__`
/// cell-name prefix.
pub const SKY130_JSON: &str = include_str!(concat!(env!("OUT_DIR"), "/sky130.cellir.json"));

/// AIGPDK (internal cell library) descriptor, generated from the in-repo
/// `aigpdk/aigpdk.lib`. AIGPDK cells have no common vendor prefix (bare `INV`,
/// `BUF`, `AND2_*`, …), so this descriptor declares no D8 selection prefix and
/// is the **no-prefix-match default fallback** (see [`default_fallback`]) —
/// mirroring `pdk::resolve_library`, where AIGPDK is already the default.
pub const AIGPDK_JSON: &str = include_str!(concat!(env!("OUT_DIR"), "/aigpdk.cellir.json"));

/// IHP SG13G2 descriptor, generated from
/// `vendor/IHP-Open-PDK/…/sg13g2_stdcell_typ_1p20V_25C.lib` at the
/// `typ_1p20V_25C` corner (read from the Liberty PVT groups). Selected by the
/// `sg13g2_` cell-name prefix. This is the ADR 0019 D7a proof: IHP is a **new**
/// built-in PDK added with **zero per-PDK Rust** — vendoring the submodule and
/// embedding this descriptor is the whole change; the combinational splice picks
/// it up through the same PDK-neutral auto-select path as every other PDK.
pub const SG13G2_JSON: &str = include_str!(concat!(env!("OUT_DIR"), "/sg13g2.cellir.json"));

/// A bundled descriptor's stable selector name and embedded JSON, paired so
/// callers can list, match, or load them uniformly.
pub struct BundledDescriptor {
    /// Stable selector accepted by `--bundled-descriptor`.
    pub name: &'static str,
    /// The embedded JSON (the byte-for-byte build-time artifact).
    pub json: &'static str,
    /// When `true`, this descriptor is the no-prefix-match default fallback
    /// (AIGPDK): it declares no D8 selection prefix, never participates in
    /// prefix auto-matching, and is selected only when no other descriptor's
    /// declared prefix matches the netlist. See [`default_fallback`].
    pub is_default_fallback: bool,
}

/// All descriptors embedded at build time. Order is stable. Prefix-keyed
/// descriptors (GF180 7t/9t, SKY130) auto-match by declared D8 prefix; the
/// `is_default_fallback` AIGPDK entry matches no prefix and is the fallback.
pub const ALL: &[BundledDescriptor] = &[
    BundledDescriptor {
        name: "gf180mcu_7t",
        json: GF180MCU_7T_JSON,
        is_default_fallback: false,
    },
    BundledDescriptor {
        name: "gf180mcu_9t",
        json: GF180MCU_9T_JSON,
        is_default_fallback: false,
    },
    BundledDescriptor {
        name: "sky130",
        json: SKY130_JSON,
        is_default_fallback: false,
    },
    BundledDescriptor {
        name: "sg13g2",
        json: SG13G2_JSON,
        is_default_fallback: false,
    },
    BundledDescriptor {
        name: "aigpdk",
        json: AIGPDK_JSON,
        is_default_fallback: true,
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

/// Auto-select a bundled descriptor by matching a netlist's cell-type names
/// against each descriptor's declared D8 `library.prefixes` (ADR 0019 D8).
///
/// This is the automatic tier of the C3.2 precedence chain, sitting *below*
/// the explicit `--cell-descriptor <file>` and `--bundled-descriptor <name>`
/// selectors handled by [`resolve`] and *above* the legacy per-PDK Rust path.
/// It replaces `pdk::PdkVariant`'s hardcoded prefix detection for the descriptor
/// choice: instead of collapsing every GF180 cell to one variant (the #130 bug,
/// where a 9t netlist was served by 7t models), it keys on the full vendor
/// prefix — which carries the track designation (`…mcu7t5v0__` vs `…mcu9t5v0__`)
/// — so a 9t netlist auto-selects the 9t descriptor.
///
/// Outcomes:
/// - `Ok(Some(ir))` — exactly one bundled descriptor's declared prefixes cover
///   the netlist; its parsed [`CellModelIr`] is returned.
/// - `Ok(None)` — no bundled descriptor declares a prefix the netlist uses
///   (e.g. an AIGPDK or SKY130 netlist, or a submodule-absent build whose
///   embedded descriptors declare no prefixes). The caller falls back to the
///   legacy per-PDK path, keeping today's behaviour.
/// - `Err(msg)` — the netlist matches more than one bundled descriptor (e.g. a
///   netlist mixing GF180 7t and 9t cells). That can't be auto-resolved, so the
///   message tells the user to pin the choice with `--bundled-descriptor` or
///   `--cell-descriptor`.
pub fn auto_select<'a>(
    cell_types: impl Iterator<Item = &'a str>,
) -> Result<Option<CellModelIr>, String> {
    // Parse each bundled descriptor once (there are only a handful). A
    // submodule-absent build embeds valid *empty* descriptors that declare no
    // prefixes; those match nothing and drop out below.
    let mut parsed: Vec<(&'static str, CellModelIr)> = ALL
        .iter()
        .map(|d| {
            (
                d.name,
                load(d.name).expect("listed bundled descriptor must load"),
            )
        })
        .collect();

    // Collect distinct cell types so the match scan is O(types × descriptors)
    // rather than O(cells × descriptors) on a million-instance netlist.
    let mut distinct: Vec<&str> = cell_types.collect();
    distinct.sort_unstable();
    distinct.dedup();

    // A descriptor matches when any netlist cell type starts with one of its
    // declared (non-empty) D8 prefixes.
    let matched: Vec<usize> = parsed
        .iter()
        .enumerate()
        .filter(|(_, (_, ir))| {
            ir.library
                .prefixes
                .iter()
                .any(|p| !p.is_empty() && distinct.iter().any(|ct| ct.starts_with(p.as_str())))
        })
        .map(|(i, _)| i)
        .collect();

    match matched.as_slice() {
        [] => Ok(None),
        &[i] => {
            let (name, ir) = parsed.swap_remove(i);
            clilog::info!(
                "Auto-selected bundled cell-model-IR descriptor '{}' \
                 ({}) from netlist cell-name prefix",
                name,
                ir.library.name
            );
            Ok(Some(ir))
        }
        many => {
            let names: Vec<&str> = many.iter().map(|&i| parsed[i].0).collect();
            Err(format!(
                "netlist cell-name prefixes match multiple bundled descriptors \
                 ({}); cannot auto-select. Pin the choice with \
                 --bundled-descriptor <name> or --cell-descriptor <file>.",
                names.join(", ")
            ))
        }
    }
}

/// The no-prefix-match default-fallback descriptor (AIGPDK).
///
/// AIGPDK cells (`INV`, `BUF`, `AND2_*`, `DFF`, …) carry no common vendor
/// prefix, so they cannot be auto-selected by [`auto_select`] (which keys on
/// declared D8 prefixes). Instead, AIGPDK is the descriptor a run consumes when
/// no prefix-keyed descriptor matches — mirroring `pdk::resolve_library`, where
/// AIGPDK is already the default verdict. Returns `None` only if the embedded
/// AIGPDK descriptor is empty (it never is — `aigpdk/aigpdk.lib` is in-repo, so
/// this is always `Some` in a real build).
pub fn default_fallback() -> Option<CellModelIr> {
    let entry = ALL.iter().find(|d| d.is_default_fallback)?;
    let ir = load(entry.name).expect("default-fallback descriptor must load");
    if ir.cells.is_empty() {
        return None;
    }
    Some(ir)
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

            // A build without a vendored submodule (GF180 / SKY130) embeds a
            // valid *empty* descriptor (see build.rs). The real-data assertions
            // only apply when the submodule was present at build time; the Unit
            // Tests CI job checks the submodules out, so they are live there.
            // (The AIGPDK fallback is in-repo and so is never empty.)
            if ir.cells.is_empty() {
                eprintln!(
                    "skipping non-empty assertions for {}: empty descriptor \
                     (vendored submodule absent at build time)",
                    d.name
                );
                continue;
            }

            if d.is_default_fallback {
                // AIGPDK: a small internal library (11 cells) with NO common
                // vendor prefix — it is the no-prefix-match default fallback, so
                // it deliberately declares no D8 selection prefix.
                assert!(
                    !ir.cells.is_empty(),
                    "{} default-fallback descriptor is empty",
                    d.name
                );
                assert!(
                    ir.library.prefixes.is_empty(),
                    "{} default-fallback descriptor must declare NO D8 prefix",
                    d.name
                );
            } else {
                // Prefix-keyed descriptors (GF180 7t/9t, SKY130, IHP SG13G2)
                // are full standard-cell libraries selected by their declared
                // prefix. Cell counts vary by library size (IHP SG13G2 is a
                // compact 84-cell library; GF180/SKY130 are 200–430), so the
                // floor is a modest sanity bound, not a per-library exact count.
                assert!(
                    ir.cells.len() > 50,
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
    }

    #[test]
    fn default_fallback_is_aigpdk_with_no_prefix() {
        // The default fallback is present in ALL and flagged.
        let entry = ALL
            .iter()
            .find(|d| d.is_default_fallback)
            .expect("ALL must contain a default-fallback descriptor");
        assert_eq!(entry.name, "aigpdk");

        // It loads, carries cells, and declares no D8 prefix (so it never
        // participates in prefix auto-matching).
        let ir = default_fallback().expect("AIGPDK fallback is in-repo, never empty");
        assert!(!ir.cells.is_empty());
        assert!(ir.library.prefixes.is_empty());

        // An AIGPDK-shaped netlist therefore gets `Ok(None)` from auto_select
        // (no prefix match) and the caller resolves the fallback instead.
        let cells = ["AND2_00_0", "INV", "BUF", "DFF"];
        assert_eq!(auto_select(cells.iter().copied()), Ok(None));
    }

    #[test]
    fn sky130_auto_selects_by_prefix() {
        // SKY130 may be empty in a submodule-absent build; only assert when its
        // declared prefix is present.
        let sky = load("sky130").expect("sky130 descriptor must load");
        if sky.library.prefixes.is_empty() {
            eprintln!("skipping: sky130 submodule absent at build time");
            return;
        }
        let cells = [
            "sky130_fd_sc_hd__inv_2",
            "sky130_fd_sc_hd__nand2_1",
            "sky130_fd_sc_hd__dfxtp_1",
        ];
        let ir = auto_select(cells.iter().copied())
            .expect("sky130 netlist must auto-match without ambiguity")
            .expect("sky130 netlist must match a bundled descriptor");
        assert!(ir.library.name.starts_with("sky130_fd_sc_hd__"));
    }

    #[test]
    fn sg13g2_auto_selects_by_prefix() {
        // IHP SG13G2 (ADR 0019 D7a, C3a): a NEW built-in PDK added with zero
        // per-PDK Rust — it must auto-select through the same prefix path as
        // every other PDK. May be empty in a submodule-absent build; only assert
        // when its declared prefix is present.
        let ihp = load("sg13g2").expect("sg13g2 descriptor must load");
        if ihp.library.prefixes.is_empty() {
            eprintln!("skipping: IHP-Open-PDK submodule absent at build time");
            return;
        }
        let cells = ["sg13g2_nand2_1", "sg13g2_inv_1", "sg13g2_mux2_1"];
        let ir = auto_select(cells.iter().copied())
            .expect("sg13g2 netlist must auto-match without ambiguity")
            .expect("sg13g2 netlist must match a bundled descriptor");
        assert!(ir.library.prefixes.iter().any(|p| p == "sg13g2_"));
    }

    #[test]
    fn unknown_name_is_none() {
        assert!(load("nonexistent_pdk").is_none());
        assert!(json_by_name("nonexistent_pdk").is_none());
    }

    /// True only when the GF180 submodules were present at build time, so the
    /// embedded GF180 descriptors carry real D8 prefixes. In a submodule-absent
    /// build (some CI jobs) those descriptors are empty and auto-match is a
    /// no-op; the real-data assertions are skipped there (mirrors
    /// `embedded_descriptors_parse_and_are_nonempty`). Scoped to the GF180
    /// descriptors specifically — the AIGPDK fallback deliberately has no prefix
    /// and SKY130's presence is independent of GF180's.
    fn descriptors_have_prefixes() -> bool {
        ["gf180mcu_7t", "gf180mcu_9t"]
            .iter()
            .all(|n| !load(n).unwrap().library.prefixes.is_empty())
    }

    #[test]
    fn auto_select_picks_9t_for_a_9t_netlist() {
        if !descriptors_have_prefixes() {
            eprintln!("skipping: GF180 submodules absent at build time");
            return;
        }
        // A 9t netlist's cell types carry the 9t track in their prefix; the 9t
        // descriptor's declared prefix is `gf180mcu_fd_sc_mcu9t5v0__`, the 7t's
        // is `gf180mcu_fd_sc_mcu7t5v0__` — disjoint, so exactly one matches.
        let cells = [
            "gf180mcu_fd_sc_mcu9t5v0__inv_2",
            "gf180mcu_fd_sc_mcu9t5v0__nand2_1",
            "gf180mcu_fd_sc_mcu9t5v0__dffq_1",
        ];
        let ir = auto_select(cells.iter().copied())
            .expect("9t netlist must auto-match without ambiguity")
            .expect("9t netlist must match a bundled descriptor");
        assert_eq!(ir.library.name, "gf180mcu_fd_sc_mcu9t5v0__tt_025C_5v00");
    }

    #[test]
    fn auto_select_picks_7t_for_a_7t_netlist() {
        if !descriptors_have_prefixes() {
            eprintln!("skipping: GF180 submodules absent at build time");
            return;
        }
        let cells = [
            "gf180mcu_fd_sc_mcu7t5v0__inv_2",
            "gf180mcu_fd_sc_mcu7t5v0__nand2_1",
        ];
        let ir = auto_select(cells.iter().copied())
            .expect("7t netlist must auto-match without ambiguity")
            .expect("7t netlist must match a bundled descriptor");
        assert_eq!(ir.library.name, "gf180mcu_fd_sc_mcu7t5v0__tt_025C_5v00");
    }

    #[test]
    fn auto_select_no_match_for_aigpdk() {
        // AIGPDK / unknown cells declare no GF180 prefix → no bundled match,
        // caller falls back to the legacy per-PDK path.
        let cells = ["AND2_00_0", "INV", "DFF"];
        let r = auto_select(cells.iter().copied());
        assert_eq!(r, Ok(None));
    }

    #[test]
    fn auto_select_mixed_tracks_are_ambiguous() {
        if !descriptors_have_prefixes() {
            eprintln!("skipping: GF180 submodules absent at build time");
            return;
        }
        // A netlist mixing 7t and 9t cells matches BOTH descriptors; neither can
        // be auto-chosen, so the caller must pin the choice explicitly.
        let cells = [
            "gf180mcu_fd_sc_mcu7t5v0__inv_2",
            "gf180mcu_fd_sc_mcu9t5v0__nand2_1",
        ];
        let r = auto_select(cells.iter().copied());
        assert!(
            r.is_err(),
            "mixed-track netlist must be ambiguous, got {r:?}"
        );
        let msg = r.unwrap_err();
        assert!(msg.contains("gf180mcu_7t") && msg.contains("gf180mcu_9t"));
        assert!(msg.contains("--bundled-descriptor"));
    }
}
