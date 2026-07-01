//! this build script compiles GEM kernels
// SPDX-FileCopyrightText: Copyright (c) 2024 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeMap;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=csrc");

    // GF180MCU: comma-separated module-port list, `input/output A, B, C;`
    // declarations, no power pins in the model.
    generate_pin_table(PinTableSpec {
        out_filename: "gf180mcu_pins.rs",
        static_name: "GF180MCU_PIN_TABLE",
        roots: &[
            "vendor/gf180mcu_fd_sc_mcu7t5v0/cells",
            "vendor/gf180mcu_fd_sc_mcu9t5v0/cells",
        ],
        prefixes: &["gf180mcu_fd_sc_mcu7t5v0__", "gf180mcu_fd_sc_mcu9t5v0__"],
        skip_pins: &[],
    });

    // SKY130: `.functional.v` carries the base (drive-free) module name
    // and one port per line. The vendored model has no power pins, so
    // `skip_pins` is empty for the same shape as gf180.
    generate_pin_table(PinTableSpec {
        out_filename: "sky130_pins.rs",
        static_name: "SKY130_PIN_TABLE",
        roots: &["vendor/sky130_fd_sc_hd/cells"],
        prefixes: &[
            "sky130_fd_sc_hd__",
            "sky130_fd_sc_hs__",
            "sky130_fd_sc_ms__",
            "sky130_fd_sc_ls__",
            "sky130_fd_sc_lp__",
            "sky130_fd_sc_hdll__",
            "sky130_fd_sc_hvl__",
        ],
        skip_pins: &[],
    });

    // ADR 0019 D7: generate the bundled cell-model-IR descriptors at build
    // time from the pinned vendored PDKs and embed them into the binary (NOT
    // checked in). Sits alongside the pin-table generators above in C3.1; the
    // pin-table step retires in the C3.3 cutover. See `generate_cell_descriptors`.
    generate_cell_descriptors();

    // Build the C++ SPI flash model
    cc::Build::new()
        .cpp(true)
        .file("csrc/spiflash_model.cc")
        .include("csrc")
        .compile("spiflash_model");

    #[cfg(feature = "cuda")]
    {
        println!("Building CUDA source files for GEM...");
        let csrc_headers = ucc::import_csrc();
        // CUDA target architecture.
        //
        // `JACQUARD_CUDA_ARCH`, when set, is passed straight through to nvcc as
        // `-arch=<value>` and overrides ucc's numeric gencode/PTX defaults.
        // Accepts any nvcc `-arch` value:
        //   - `native`     — build SASS for the GPU(s) in THIS machine. Best for
        //                    local dev (e.g. an `sm_120` Blackwell box); fastest
        //                    kernel, no first-load JIT, but not portable.
        //   - `all-major`  — SASS for every major arch the toolkit knows plus PTX
        //                    for the newest. Portable; use for distribution.
        //   - `sm_120` / `compute_90` / … — an explicit single target.
        // Unset → ucc defaults (PTX compute_50 + SASS sm_70/sm_80), which run on
        // newer GPUs via PTX JIT.
        println!("cargo:rerun-if-env-changed=JACQUARD_CUDA_ARCH");
        let mut cl_cuda = match std::env::var("JACQUARD_CUDA_ARCH") {
            Ok(arch) if !arch.is_empty() => {
                println!("Using CUDA -arch={arch} (JACQUARD_CUDA_ARCH)");
                let mut b = ucc::cl_cuda_arch(None, None);
                b.flag(&format!("-arch={arch}"));
                b
            }
            _ => ucc::cl_cuda(),
        };
        cl_cuda.ccbin(false);
        cl_cuda.flag("-lineinfo");
        cl_cuda.flag("-maxrregcount=128");
        cl_cuda
            .debug(false)
            .opt_level(3)
            .include(&csrc_headers)
            .files(["csrc/kernel_v1.cu"]);
        cl_cuda.compile("gemcu");
        println!("cargo:rustc-link-lib=static=gemcu");
        println!("cargo:rustc-link-lib=dylib=cudart");
        ucc::bindgen(["csrc/kernel_v1.cu"], "kernel_v1.rs");
        ucc::export_csrc();
        ucc::make_compile_commands(&[&cl_cuda]);
    }

    #[cfg(feature = "hip")]
    {
        println!("Building HIP source files for GEM...");
        let csrc_headers = ucc::import_csrc();
        let mut cl_hip = ucc::cl_hip();
        cl_hip
            .debug(false)
            .opt_level(3)
            .include(&csrc_headers)
            .file("csrc/kernel_v1.hip.cpp");
        cl_hip.compile("gemhip");
        println!("cargo:rustc-link-lib=static=gemhip");
        // On AMD backend, link amdhip64; on NVIDIA backend, link cudart.
        // The kernel_v1.hip.cpp wrapper handles both via hipcc compilation.
        if std::env::var("HIP_PLATFORM").as_deref() == Ok("nvidia") {
            println!("cargo:rustc-link-lib=dylib=cudart");
            println!("cargo:rustc-link-lib=dylib=cuda");
            let cuda_path =
                std::env::var("CUDA_PATH").unwrap_or_else(|_| "/usr/local/cuda".to_string());
            println!("cargo:rustc-link-search=native={}/lib64", cuda_path);
            println!("cargo:rustc-link-search=native={}/lib", cuda_path);
            println!("cargo:rustc-link-search=native={}/lib64/stubs", cuda_path);
            println!("cargo:rustc-link-search=native={}/lib/stubs", cuda_path);
        } else {
            println!("cargo:rustc-link-lib=dylib=amdhip64");
            let rocm_path = std::env::var("ROCM_PATH").unwrap_or_else(|_| "/opt/rocm".to_string());
            println!("cargo:rustc-link-search=native={}/lib", rocm_path);
        }
        println!("cargo:rerun-if-env-changed=HIP_PLATFORM");
        println!("cargo:rerun-if-env-changed=CUDA_PATH");
        ucc::bindgen(["csrc/kernel_v1.hip.cpp"], "kernel_v1_hip.rs");
        ucc::export_csrc();
        ucc::make_compile_commands(&[&cl_hip]);
    }

    #[cfg(feature = "metal")]
    {
        println!("Building Metal shader for GEM...");
        // Compile Metal shader to metallib
        ucc::cl_metal()
            .file("csrc/kernel_v1.metal")
            .std_version("metal3.0")
            .macos_version_min("14.0")
            .compile("gem_metal");
        // METALLIB_PATH environment variable is set by the compile step
    }
}

// -----------------------------------------------------------------------------
// Bundled cell-model-IR descriptor generation (ADR 0019 D7)
// -----------------------------------------------------------------------------
//
// Runs the `liberty-to-cellir` converter *as a library* over the pinned
// vendored GF180 PDKs and writes the resulting cell-model-IR descriptors into
// `$OUT_DIR` for `include_str!` by `src/bundled_descriptors.rs`. The
// descriptors are NOT checked in (D7) — they are a deterministic function of
// the pinned submodules.
//
// Gating: `cargo:rerun-if-changed` on each library's source directory means
// this only re-runs when the vendored PDK changes (or csrc / build.rs itself),
// so it does not balloon incremental build time. The generation itself is
// ~0.1 s per library (229 GF180 cells -> ~1.5 MB JSON).

struct DescriptorSpec {
    /// Output filename under `$OUT_DIR`, embedded via `include_str!`.
    out_filename: &'static str,
    /// Top-level Liberty (`tt` corner) the descriptor is generated from. If it
    /// carries no `cell` groups, the converter merges the sibling per-cell
    /// `.lib` split layout (GF180/SKY130 shape).
    top_lib: &'static str,
    /// Source directories to watch for `rerun-if-changed` — the top-level
    /// `liberty/` tree and the per-cell `cells/` tree.
    watch_dirs: &'static [&'static str],
}

fn generate_cell_descriptors() {
    // GF180 7-track (default) and 9-track, SKY130, and the internal AIGPDK.
    // The GF180 vendored submodules are already required by the pin-table
    // generators above, so they add no new source-build submodule requirement
    // (ADR 0019 D7); SKY130 ships only `.lib.json` (routed through the converter
    // loader's JSON path); AIGPDK is an in-repo `.lib` text library that is
    // always present.
    let specs = [
        DescriptorSpec {
            out_filename: "gf180mcu_7t.cellir.json",
            top_lib: "vendor/gf180mcu_fd_sc_mcu7t5v0/liberty/\
                      gf180mcu_fd_sc_mcu7t5v0__tt_025C_5v00.lib",
            watch_dirs: &[
                "vendor/gf180mcu_fd_sc_mcu7t5v0/liberty",
                "vendor/gf180mcu_fd_sc_mcu7t5v0/cells",
            ],
        },
        DescriptorSpec {
            out_filename: "gf180mcu_9t.cellir.json",
            top_lib: "vendor/gf180mcu_fd_sc_mcu9t5v0/liberty/\
                      gf180mcu_fd_sc_mcu9t5v0__tt_025C_5v00.lib",
            watch_dirs: &[
                "vendor/gf180mcu_fd_sc_mcu9t5v0/liberty",
                "vendor/gf180mcu_fd_sc_mcu9t5v0/cells",
            ],
        },
        // SKY130: the converter loader recognises the `.lib.json` extension and
        // assembles a per-corner library from the per-corner header plus every
        // per-cell-per-corner `cells/**/*__tt_025C_1v80.lib.json` file. The
        // submodule may be absent on a partial checkout, in which case an empty
        // descriptor is embedded (mirrors GF180 below).
        DescriptorSpec {
            out_filename: "sky130.cellir.json",
            top_lib: "vendor/sky130_fd_sc_hd/timing/\
                      sky130_fd_sc_hd__tt_025C_1v80.lib.json",
            watch_dirs: &[
                "vendor/sky130_fd_sc_hd/timing",
                "vendor/sky130_fd_sc_hd/cells",
            ],
        },
        // AIGPDK: the internal cell library, an in-repo `.lib` text file (always
        // present — not a submodule). Its cells have no common vendor prefix, so
        // it declares no D8 selection prefix and serves as the no-prefix-match
        // default fallback (wired in `bundled_descriptors`).
        DescriptorSpec {
            out_filename: "aigpdk.cellir.json",
            top_lib: "aigpdk/aigpdk.lib",
            watch_dirs: &["aigpdk"],
        },
        // IHP SG13G2 (open-source, added as a NEW built-in PDK with ZERO
        // per-PDK Rust — ADR 0019 D7a, C3a). A monolithic single-`.lib`-per-corner
        // commercial-style library (Liberate); the converter reads its `cell`
        // groups directly (no split-lib merge) and derives the `sg13g2_` D8
        // prefix from the cell names. Only the vendored stdcell Liberty is needed;
        // the submodule is sparse/shallow (stdcell `lib/` + `verilog/` only). When
        // absent on a partial checkout, an empty descriptor is embedded (as GF180).
        DescriptorSpec {
            out_filename: "sg13g2.cellir.json",
            top_lib: "vendor/IHP-Open-PDK/ihp-sg13g2/libs.ref/sg13g2_stdcell/lib/\
                      sg13g2_stdcell_typ_1p20V_25C.lib",
            watch_dirs: &["vendor/IHP-Open-PDK/ihp-sg13g2/libs.ref/sg13g2_stdcell/lib"],
        },
    ];

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");
    for spec in &specs {
        for dir in spec.watch_dirs {
            println!("cargo:rerun-if-changed={dir}");
        }
        generate_one_descriptor(spec, Path::new(&out_dir));
    }
}

fn generate_one_descriptor(spec: &DescriptorSpec, out_dir: &Path) {
    let top_lib = Path::new(spec.top_lib);
    let out_path = out_dir.join(spec.out_filename);

    // The vendored submodule may be uninitialised on a partial checkout (the
    // CUDA/HIP/Lint CI jobs deliberately skip the heavy GF180 submodules).
    // `include_str!` still needs the OUT_DIR file to exist, so mirror the
    // pin-table generator's tolerance exactly: write a valid *empty*
    // descriptor and let the consumer's runtime surface the missing-library
    // error if a GF180 design is actually run. The Unit Tests + jtag cosim CI
    // jobs (which DO check out GF180) cover the real-data path. Source builds
    // keep the same vendored-submodule requirement they already have (D7).
    if !top_lib.exists() {
        let stem = top_lib
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");
        let empty = cell_model_ir::CellModelIr::new(cell_model_ir::LibraryMeta {
            name: stem.to_string(),
            prefixes: Vec::new(),
        });
        let json = empty.to_json().expect("serialize empty descriptor");
        std::fs::write(&out_path, &json)
            .unwrap_or_else(|e| panic!("writing {}: {e}", out_path.display()));
        println!(
            "cargo:warning=cell-model-IR: {} not found — embedded an EMPTY {} \
             descriptor (run `git submodule update --init` for the real one)",
            top_lib.display(),
            spec.out_filename
        );
        return;
    }

    let started = std::time::Instant::now();
    // Empty prefix => derive the D8 selection prefix from the cell names.
    let ir = liberty_to_cellir::load::generate_descriptor(top_lib, Vec::new())
        .unwrap_or_else(|e| panic!("generating {}: {e}", spec.out_filename));
    let json = ir
        .to_json()
        .unwrap_or_else(|e| panic!("serializing {}: {e}", spec.out_filename));
    std::fs::write(&out_path, &json)
        .unwrap_or_else(|e| panic!("writing {}: {e}", out_path.display()));
    let elapsed = started.elapsed();
    println!(
        "cargo:warning=cell-model-IR: generated {} ({} cells, {} bytes) in {:.2}s",
        spec.out_filename,
        ir.cells.len(),
        json.len(),
        elapsed.as_secs_f64()
    );
}

// -----------------------------------------------------------------------------
// PDK pin-direction table generator
// -----------------------------------------------------------------------------
//
// Scans vendored standard-cell submodules and extracts each cell's port
// directions from its `.functional.v` model. Writes
// `$OUT_DIR/<spec.out_filename>` for `include!` by the matching PDK module
// (`src/gf180mcu.rs`, `src/sky130.rs`).
//
// Drive strengths within a cell type share port layouts; the generator
// dedupes them and panics on disagreement (a real Liberty regression).
// The two PDKs share grammar shape (Verilog 95: `module name( ... );`
// followed by `input/output a, b, c;` declarations) so a single
// `parse_module_ports` covers both.

struct PinTableSpec {
    /// Filename to emit under `$OUT_DIR`.
    out_filename: &'static str,
    /// Name of the generated `&[(&str, &[(&str, Direction)])]` static.
    static_name: &'static str,
    /// Roots to scan for `<cell>/<module>.functional.v` files.
    roots: &'static [&'static str],
    /// Library prefixes to strip from module names before keying.
    prefixes: &'static [&'static str],
    /// Pin names to drop from the emitted table (e.g. power pins on
    /// PDKs whose `.functional.v` exposes them — currently neither
    /// gf180mcu nor sky130 do, but the hook is here for future PDKs).
    skip_pins: &'static [&'static str],
}

fn generate_pin_table(spec: PinTableSpec) {
    for root in spec.roots {
        println!("cargo:rerun-if-changed={root}");
    }

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");
    let out_path = Path::new(&out_dir).join(spec.out_filename);

    let mut table: BTreeMap<String, (Vec<String>, Vec<String>)> = BTreeMap::new();
    for variant_root in spec.roots {
        let root = Path::new(variant_root);
        if !root.is_dir() {
            // Submodule uninitialised — leave the table empty and let the
            // consumer's runtime panic surface a clear error. CI / docs
            // already document `git submodule update --init`.
            continue;
        }
        for cell_dir in std::fs::read_dir(root).expect("read cells dir").flatten() {
            let path = cell_dir.path();
            if !path.is_dir() {
                continue;
            }
            for entry in std::fs::read_dir(&path)
                .expect("read cell variant dir")
                .flatten()
            {
                let f = entry.path();
                let Some(name) = f.file_name().and_then(|s| s.to_str()) else {
                    continue;
                };
                // .functional.v is the authoritative port-list source; skip
                // the preprocessed (.pp.v) variant to avoid duplicate work.
                if !name.ends_with(".functional.v") || name.ends_with(".functional.pp.v") {
                    continue;
                }
                let body = std::fs::read_to_string(&f).expect("read functional.v");
                let Some((macro_name, inputs, outputs)) = parse_module_ports(&body) else {
                    continue;
                };
                let cell_type = base_cell_type(&macro_name, spec.prefixes).to_string();
                let inputs = filter_skip(inputs, spec.skip_pins);
                let outputs = filter_skip(outputs, spec.skip_pins);
                match table.get(&cell_type) {
                    None => {
                        table.insert(cell_type, (inputs, outputs));
                    }
                    Some((existing_i, existing_o)) => {
                        // Drive variants and (where applicable) variant
                        // submodules must agree — otherwise the assumption
                        // "keyed by base type" is broken and we want to
                        // know loudly.
                        assert_eq!(
                            existing_i, &inputs,
                            "input pin mismatch for cell type {cell_type}",
                        );
                        assert_eq!(
                            existing_o, &outputs,
                            "output pin mismatch for cell type {cell_type}",
                        );
                    }
                }
            }
        }
    }

    let mut out = String::new();
    out.push_str(&format!(
        "// Generated by build.rs — do not edit.\n// Sources: {}\n\n",
        spec.roots.join(", ")
    ));
    out.push_str(&format!(
        "pub(crate) static {}: &[(&str, &[(&str, Direction)])] = &[\n",
        spec.static_name
    ));
    for (cell_type, (inputs, outputs)) in &table {
        out.push_str(&format!("    (\"{cell_type}\", &[\n"));
        for pin in inputs {
            out.push_str(&format!("        (\"{pin}\", Direction::I),\n"));
        }
        for pin in outputs {
            out.push_str(&format!("        (\"{pin}\", Direction::O),\n"));
        }
        out.push_str("    ]),\n");
    }
    out.push_str("];\n");

    std::fs::write(&out_path, out).expect("write pin table");
}

fn filter_skip(pins: Vec<String>, skip: &[&str]) -> Vec<String> {
    if skip.is_empty() {
        return pins;
    }
    pins.into_iter()
        .filter(|p| !skip.contains(&p.as_str()))
        .collect()
}

/// Parse one `.functional.v` cell model. Returns (module_name, inputs, outputs).
///
/// Grammar shared by GF180MCU and SKY130 PDKs: a single
/// `module <name>( ... );` header followed by `input <...>;` and
/// `output <...>;` lines. The body may have comma-separated port names
/// (gf180) or one port per line (sky130) — both work because we trim
/// and split on commas regardless.
fn parse_module_ports(body: &str) -> Option<(String, Vec<String>, Vec<String>)> {
    let mut module_name: Option<String> = None;
    let mut inputs = Vec::new();
    let mut outputs = Vec::new();

    for line in body.lines() {
        let line = line.trim();
        if module_name.is_none() {
            if let Some(rest) = line.strip_prefix("module ") {
                if let Some(paren) = rest.find('(') {
                    module_name = Some(rest[..paren].trim().to_string());
                }
            }
        }
        // SKY130 uses two spaces ("input  A;") so we accept any whitespace
        // after the keyword via `strip_prefix("input ")` + trim further down.
        if let Some(rest) = line.strip_prefix("input ") {
            push_port_names(rest, &mut inputs);
        } else if let Some(rest) = line.strip_prefix("output ") {
            push_port_names(rest, &mut outputs);
        }
    }

    module_name.map(|n| (n, inputs, outputs))
}

fn push_port_names(decl: &str, out: &mut Vec<String>) {
    let cleaned = decl.trim_end_matches(';').trim();
    for name in cleaned.split(',') {
        let name = name.trim();
        if !name.is_empty() {
            out.push(name.to_string());
        }
    }
}

/// Drop the library prefix (one of `spec.prefixes`) and a trailing
/// numeric drive-strength suffix to recover the base cell type. Mirrors
/// `src/gf180mcu.rs::extract_cell_type` and `src/sky130.rs::extract_cell_type`
/// — kept in sync by the assert_eq cross-check in the per-cell-type
/// dedup pass above (drive variants must produce the same key).
fn base_cell_type<'a>(macro_name: &'a str, prefixes: &[&str]) -> &'a str {
    let mut stripped = macro_name;
    for prefix in prefixes {
        if let Some(rest) = macro_name.strip_prefix(prefix) {
            stripped = rest;
            break;
        }
    }
    if let Some(idx) = stripped.rfind('_') {
        let suffix = &stripped[idx + 1..];
        if !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()) {
            return &stripped[..idx];
        }
    }
    stripped
}
