// SPDX-License-Identifier: Apache-2.0

//! Determinism gate for build-time descriptor generation (ADR 0019 D7).
//!
//! D7 requires bundled descriptors to be **regenerated at build time and
//! byte-identical across repeated builds**. The generator emits no `HashMap`
//! into the descriptor (cells/pins/arcs are `Vec`s built in Liberty group
//! order, and split-library file discovery `sort()`s before merge), so this
//! test pins that property: convert the GF180 7t library twice and assert the
//! serialized JSON is byte-identical.
//!
//! Skips gracefully when the vendored GF180 submodule is absent (local dev
//! without `git submodule update`); CI checks the submodules out, so the gate
//! is live there.

use std::path::{Path, PathBuf};

use liberty_to_cellir::load::generate_descriptor;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repo root above crates/liberty-to-cellir")
        .to_path_buf()
}

/// The top-level GF180 7t typical-corner Liberty the build embeds (ADR 0019
/// D7). Matches `build.rs`'s `gf180mcu_7t` generation input.
fn gf180_7t_tt_lib() -> PathBuf {
    repo_root()
        .join("vendor/gf180mcu_fd_sc_mcu7t5v0/liberty/gf180mcu_fd_sc_mcu7t5v0__tt_025C_5v00.lib")
}

/// SKY130's typical-corner `.lib.json` header (the per-corner library header;
/// cells are discovered alongside under `cells/`).
fn sky130_tt_header() -> PathBuf {
    repo_root().join("vendor/sky130_fd_sc_hd/timing/sky130_fd_sc_hd__tt_025C_1v80.lib.json")
}

#[test]
fn gf180_7t_descriptor_is_byte_deterministic() {
    let lib = gf180_7t_tt_lib();
    if !lib.exists() {
        eprintln!(
            "skipping: vendored GF180 7t submodule absent at {} \
             (run `git submodule update --init`)",
            lib.display()
        );
        return;
    }

    let a = generate_descriptor(&lib, Vec::new())
        .expect("first generation")
        .to_json()
        .expect("serialize first");
    let b = generate_descriptor(&lib, Vec::new())
        .expect("second generation")
        .to_json()
        .expect("serialize second");

    assert_eq!(
        a.len(),
        b.len(),
        "regenerated descriptor changed length: {} vs {}",
        a.len(),
        b.len()
    );
    assert!(
        a == b,
        "regenerated GF180 7t descriptor is not byte-identical — a HashMap \
         likely leaked into the emitted collections (ADR 0019 D7 determinism)"
    );

    // Sanity: a real, non-empty descriptor (not the empty-library fallback).
    let ir = generate_descriptor(&lib, Vec::new()).unwrap();
    assert!(
        ir.cells.len() > 100,
        "expected the full GF180 7t cell set, got {} cells",
        ir.cells.len()
    );
    assert!(
        !ir.library.prefixes.is_empty(),
        "descriptor should declare a D8 selection prefix"
    );
}

#[test]
fn sky130_lib_json_assembles_a_real_descriptor() {
    let header = sky130_tt_header();
    if !header.exists() {
        eprintln!(
            "skipping: vendored SKY130 submodule absent at {} \
             (run `git submodule update --init`)",
            header.display()
        );
        return;
    }

    // Byte-deterministic across runs (the `.lib.json` cell discovery `sort()`s).
    let a = generate_descriptor(&header, Vec::new())
        .expect("first SKY130 generation")
        .to_json()
        .expect("serialize first");
    let b = generate_descriptor(&header, Vec::new())
        .expect("second SKY130 generation")
        .to_json()
        .expect("serialize second");
    assert_eq!(a, b, "SKY130 `.lib.json` descriptor is not byte-identical");

    let ir = generate_descriptor(&header, Vec::new()).unwrap();

    // A real multi-hundred-cell library assembled from the JSON cell files.
    assert!(
        ir.cells.len() > 300,
        "expected the SKY130 cell set, got {} cells",
        ir.cells.len()
    );
    assert_eq!(
        ir.library.prefixes,
        vec!["sky130_fd_sc_hd__".to_string()],
        "SKY130 descriptor should declare the vendor prefix"
    );

    // Corner is Liberty-derived from the header's operating_conditions PVT.
    assert_eq!(ir.corners.len(), 1);
    let corner = &ir.corners[0];
    assert_eq!(corner.name, "tt_025C_1v80");
    assert_eq!(corner.process, "tt");
    assert_eq!(corner.voltage, 1.8);
    assert_eq!(corner.temperature, 25.0);

    // Spot-check a combinational cell (inv ⇒ Y = !A) and a sequential one.
    let find = |t: &str| ir.cells.iter().find(|c| c.cell_type == t);
    let inv = find("sky130_fd_sc_hd__inv_1").expect("inv_1 present");
    let logic = inv.logic.as_ref().expect("inv_1 has combinational logic");
    use std::collections::HashMap;
    for a in [false, true] {
        let mut vals = HashMap::new();
        vals.insert("A".to_string(), a);
        assert_eq!(logic.eval(&vals).unwrap()["Y"], !a, "inv Y == !A");
    }

    let dff = find("sky130_fd_sc_hd__dfxtp_1").expect("dfxtp_1 present");
    assert!(dff.sequential.is_some(), "dfxtp_1 has L3 sequential");
    assert!(dff.timing.is_some(), "dfxtp_1 has L4 timing");

    // L4 timing scaled to true picoseconds via the restored `1ns` time_unit:
    // the CLK->Q delay is in the tens-to-hundreds of ps, not sub-ps.
    let t = dff.timing.as_ref().unwrap();
    let clk_q = t
        .delays
        .iter()
        .find(|d| d.from_pin == "CLK" && d.to_pin == "Q")
        .expect("CLK->Q delay");
    let typ = clk_q.rise[0].typ;
    assert!(
        typ > 10.0 && typ < 10_000.0,
        "CLK->Q delay {typ} ps is not in a realistic ps range (time_unit mis-scaled?)"
    );
}
