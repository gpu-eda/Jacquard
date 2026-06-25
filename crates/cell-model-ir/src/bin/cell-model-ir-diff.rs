//! `cell-model-ir-diff` — structural diff between two cell-model-IR documents.
//!
//! Used by CI to validate that descriptor regeneration is deterministic
//! (ADR 0019 D7): a freshly generated descriptor must structurally equal the
//! previous one. Also useful for debugging differences between descriptors
//! produced by different converter versions.

use std::path::PathBuf;
use std::process::ExitCode;

use cell_model_ir::diff::diff_irs;
use cell_model_ir::CellModelIr;
use clap::Parser;

/// Exit codes (mirroring `timing-ir-diff`).
const EXIT_CLEAN: u8 = 0;
const EXIT_DIFFS: u8 = 1;
const EXIT_ERROR: u8 = 2;

#[derive(Parser, Debug)]
#[command(
    name = "cell-model-ir-diff",
    about = "Diff two Jacquard cell-model-IR (JSON) files",
    long_about = "Produces a structural diff between two cell-model-IR documents. \
                  Used by CI to validate that descriptor regeneration is deterministic."
)]
struct Args {
    /// Left-hand input. Typically the "expected" / previous descriptor.
    a: PathBuf,
    /// Right-hand input. Typically the freshly regenerated descriptor.
    b: PathBuf,
}

fn main() -> ExitCode {
    let args = Args::parse();

    let a = match CellModelIr::read_from(&args.a) {
        Ok(ir) => ir,
        Err(e) => {
            eprintln!("error reading {}: {e}", args.a.display());
            return ExitCode::from(EXIT_ERROR);
        }
    };
    let b = match CellModelIr::read_from(&args.b) {
        Ok(ir) => ir,
        Err(e) => {
            eprintln!("error reading {}: {e}", args.b.display());
            return ExitCode::from(EXIT_ERROR);
        }
    };

    let d = diff_irs(&a, &b);
    if d.is_clean() {
        println!(
            "clean: {} and {} are structurally identical",
            args.a.display(),
            args.b.display()
        );
        ExitCode::from(EXIT_CLEAN)
    } else {
        println!("{} mismatch(es):", d.mismatches.len());
        for m in &d.mismatches {
            println!("  {m}");
        }
        ExitCode::from(EXIT_DIFFS)
    }
}
