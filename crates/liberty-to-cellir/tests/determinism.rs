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
