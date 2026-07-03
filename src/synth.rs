//! Embedded synthesis on-ramp — behavioral RTL → gate-level aigpdk netlist.
//!
//! Runs YoWASP's Yosys (a single self-contained `yosys.wasm`, abc compiled
//! in-tree and called in-process) directly from Rust via `wasmtime` — no Python
//! interpreter and no external toolchain. See
//! [ADR 0021](../docs/adr/0021-behavioral-rtl-support.md) and the proving spike
//! at `docs/spikes/rust-wasmtime-yosys/`.
//!
//! This is a *pre-processor* invoked transparently by `jacquard sim`/`cosim`
//! when handed behavioral RTL (ADR 0021 §1): it produces the same structural
//! aigpdk netlist a user would synthesize by hand (`docs/synthesis-flow.md`),
//! which then feeds the emulator pipeline unchanged. The synthesized netlist is
//! cached by content hash so repeat runs skip synthesis. YoWASP Yosys is the
//! functional on-ramp; bring-your-own DC remains the performance path
//! (synthesis quality sets GPU speed).

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use wasmtime::{Engine, Linker, Module, Store};
use wasmtime_wasi::p1::{self, WasiP1Ctx};
use wasmtime_wasi::{DirPerms, FilePerms, I32Exit, WasiCtxBuilder};

// aigpdk synthesis-support files, embedded so `build` is self-contained (small;
// unlike the ~39 MB wasm these are a few KB each).
const AIGPDK_NOMEM_LIB: &str = include_str!("../aigpdk/aigpdk_nomem.lib");
const MEMLIB_YOSYS: &str = include_str!("../aigpdk/memlib_yosys.txt");
const GEM_FORMAL_V: &str = include_str!("../aigpdk/gem_formal.v");

/// Options controlling an embedded-Yosys synthesis run (ADR 0021).
pub struct SynthOptions {
    /// Explicit top module (else Yosys auto-detects via `hierarchy`).
    pub top_module: Option<String>,
    /// Explicit path to `yosys.wasm`; else `JACQUARD_YOSYS_WASM` / discovery.
    pub yosys_wasm: Option<PathBuf>,
    /// Keep assertions as `GEM_ASSERT` cells (default). When false, strip them
    /// for a pure logic netlist.
    pub keep_assertions: bool,
    /// When set (`--emit-synth`), also copy the synthesized netlist here for
    /// inspection / fixture authoring.
    pub emit_synth: Option<PathBuf>,
}

impl Default for SynthOptions {
    fn default() -> Self {
        Self {
            top_module: None,
            yosys_wasm: None,
            keep_assertions: true,
            emit_synth: None,
        }
    }
}

/// Synthesize a single behavioral RTL `design` to a gate-level aigpdk netlist,
/// returning the path to the (cached) `.gv`.
///
/// The result is cached under `$XDG_CACHE_HOME/jacquard` keyed by the content
/// hash of the design source, the generated synthesis script, and the
/// `yosys.wasm` module — so a repeat `sim`/`cosim` run of the same RTL skips
/// synthesis entirely (mirroring the compiled-module cache next to it).
pub fn synthesize(design: &Path, opts: &SynthOptions) -> Result<PathBuf> {
    let (wasm_path, share_dir) = locate_yosys_wasm(opts.yosys_wasm.as_deref())?;

    let design_bytes =
        std::fs::read(design).with_context(|| format!("reading design {}", design.display()))?;
    let wasm_bytes =
        std::fs::read(&wasm_path).with_context(|| format!("reading {}", wasm_path.display()))?;

    // Pick the SystemVerilog frontend once per wasm module (probe + cache).
    let use_slang = read_slang_available(&wasm_path, &share_dir);

    // The design is copied into the work dir under its basename; the script
    // references that basename.
    let base = design
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "input.v".to_string());
    let script = synth_script(
        std::slice::from_ref(&base),
        opts.top_module.as_deref(),
        opts.keep_assertions,
        use_slang,
    );

    // Cache key: design source + generated script + wasm module (+ crate
    // version, to invalidate across incompatible synth-engine changes).
    let cache_key = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        design_bytes.hash(&mut h);
        script.hash(&mut h);
        wasm_bytes.hash(&mut h);
        env!("CARGO_PKG_VERSION").hash(&mut h);
        format!("synth-{:016x}.gv", h.finish())
    };
    let cache_dir = cache_dir();
    let cached = cache_dir.join(&cache_key);

    if cached.is_file() {
        clilog::info!("synth: cache hit {}", cached.display());
    } else {
        clilog::info!("synth: using yosys.wasm at {}", wasm_path.display());
        // Everything Yosys reads/writes lives in one temp dir, preopened as cwd.
        // This avoids WASI path-escaping and keeps a single preopen for inputs.
        let work =
            tempdir::TempDir::new("jacquard-synth").context("creating temp work directory")?;
        let wp = work.path();
        std::fs::write(wp.join("aigpdk_nomem.lib"), AIGPDK_NOMEM_LIB)?;
        std::fs::write(wp.join("memlib_yosys.txt"), MEMLIB_YOSYS)?;
        std::fs::write(wp.join("gem_formal.v"), GEM_FORMAL_V)?;
        std::fs::write(wp.join(&base), &design_bytes)?;
        std::fs::write(wp.join("synth.ys"), &script)?;

        run_yosys_wasm(&wasm_path, &share_dir, wp, &["yosys", "-s", "synth.ys"])
            .context("running YoWASP Yosys synthesis")?;

        let produced = wp.join("gatelevel.gv");
        if !produced.exists() {
            bail!(
                "Yosys ran but produced no gatelevel.gv — synthesis likely failed \
                 (re-run is verbose). Script:\n{script}"
            );
        }
        std::fs::create_dir_all(&cache_dir).ok();
        std::fs::copy(&produced, &cached)
            .with_context(|| format!("caching synthesized netlist to {}", cached.display()))?;
    }

    if let Some(emit) = &opts.emit_synth {
        if let Some(parent) = emit.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).ok();
            }
        }
        std::fs::copy(&cached, emit)
            .with_context(|| format!("writing --emit-synth {}", emit.display()))?;
        clilog::info!("synth: wrote intermediate netlist to {}", emit.display());
    }

    Ok(cached)
}

/// Whether the embedded Yosys exposes `read_slang` (yosys-slang, a
/// near-complete SV-2017 elaborator). Probed once via `help read_slang` and
/// cached for the process; falls back to `read_verilog -sv` when slang is
/// absent (an older wasm) so the on-ramp degrades gracefully.
fn read_slang_available(wasm_path: &Path, share_dir: &Path) -> bool {
    static CACHE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| match probe_help(wasm_path, share_dir, "read_slang") {
        Ok(log) => {
            // Yosys prints "No such command or cell type: read_slang" when the
            // command is absent; otherwise it prints the command's help text.
            let present = !log.contains("No such command");
            clilog::info!(
                "synth: SV frontend = {}",
                if present {
                    "read_slang (yosys-slang)"
                } else {
                    "read_verilog -sv (slang absent)"
                }
            );
            present
        }
        Err(e) => {
            clilog::warn!("synth: read_slang probe failed ({e:#}); using read_verilog -sv");
            false
        }
    })
}

/// Run `help <cmd>` and return Yosys's captured log text. Uses `yosys -l
/// <logfile>` (written into the preopened work dir) rather than WASI stdout
/// capture — robust across wasmtime-wasi API surfaces.
fn probe_help(wasm_path: &Path, share_dir: &Path, cmd: &str) -> Result<String> {
    let work = tempdir::TempDir::new("jacquard-probe").context("creating probe work dir")?;
    let wp = work.path();
    std::fs::write(wp.join("probe.ys"), format!("help {cmd}\n"))?;
    run_yosys_wasm(
        wasm_path,
        share_dir,
        wp,
        &["yosys", "-q", "-l", "probe.log", "-s", "probe.ys"],
    )
    .context("probing Yosys command set")?;
    Ok(std::fs::read_to_string(wp.join("probe.log")).unwrap_or_default())
}

/// Generate the aigpdk synthesis script (Yosys path of `docs/synthesis-flow.md`,
/// plus memory mapping and assertion lowering). Fronts SystemVerilog with
/// `read_slang` when available (`use_slang`), else `read_verilog -sv`.
fn synth_script(
    inputs: &[String],
    top: Option<&str>,
    keep_assertions: bool,
    use_slang: bool,
) -> String {
    let reads = if use_slang {
        // yosys-slang is a whole-program elaborator: read all files in one call.
        format!("read_slang {}", inputs.join(" "))
    } else {
        inputs
            .iter()
            .map(|f| format!("read_verilog -sv {f}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let top_arg = top.map(|t| format!(" -top {t}")).unwrap_or_default();
    // `hierarchy` needs a top; auto-detect when the user didn't name one.
    let hierarchy = if top.is_some() {
        format!("hierarchy -check{top_arg}")
    } else {
        "hierarchy -check -auto-top".to_string()
    };
    // Lower $assert/$assume/$cover/$check/$print -> GEM_ASSERT (aigpdk.v), so the
    // emulator sees them; or delete them for a pure netlist.
    let assertion_step = if keep_assertions {
        "techmap -map gem_formal.v"
    } else {
        "chformal -remove\n        delete t:$print"
    };
    format!(
        "\
# Generated by the jacquard sim/cosim synthesis on-ramp (ADR 0021).
# aigpdk logic + memory synthesis.
# GEM_ASSERT and aigpdk cells are emitted as blackbox instances (no defs needed).
{reads}
{hierarchy}

# Flatten + memory synthesis: recognize + map RAM blocks (memlib_yosys.txt).
flatten
proc
opt_expr; opt_dff; opt_clean
memory -nomap
memory_libmap -lib memlib_yosys.txt -logic-cost-rom 100 -logic-cost-ram 100

# Logic synthesis to aigpdk cells (begin phase already done above).
synth{top_arg} -run coarse:
{assertion_step}
dfflibmap -liberty aigpdk_nomem.lib
opt_clean -purge
abc -liberty aigpdk_nomem.lib
opt_clean -purge
techmap
abc -liberty aigpdk_nomem.lib
opt_clean -purge

write_verilog -noattr gatelevel.gv
stat
"
    )
}

/// Resolve `yosys.wasm` and its `share/` dir: explicit arg → `JACQUARD_YOSYS_WASM`
/// env → a discovered `yowasp_yosys` Python install. (Fetch-from-release is a
/// planned follow-up — see ADR 0021 / #162.)
fn locate_yosys_wasm(explicit: Option<&Path>) -> Result<(PathBuf, PathBuf)> {
    if let Some(p) = explicit {
        return with_share(p.to_path_buf());
    }
    if let Ok(env) = std::env::var("JACQUARD_YOSYS_WASM") {
        if !env.is_empty() {
            return with_share(PathBuf::from(env));
        }
    }
    if let Some(found) = discover_yowasp() {
        return Ok(found);
    }
    bail!(
        "could not find yosys.wasm. Pass --yosys-wasm <path>, set \
         JACQUARD_YOSYS_WASM, or install the YoWASP Yosys wheel \
         (`pip install yowasp-yosys`). Automatic fetch is a planned follow-up \
         (ADR 0021 / #162)."
    )
}

/// Given a `yosys.wasm` path, locate its sibling `share/` dir (the layout in the
/// YoWASP wheel: `<pkg>/yosys.wasm` + `<pkg>/share`).
fn with_share(wasm: PathBuf) -> Result<(PathBuf, PathBuf)> {
    if !wasm.is_file() {
        bail!("yosys.wasm not found at {}", wasm.display());
    }
    let share = wasm
        .parent()
        .map(|d| d.join("share"))
        .filter(|s| s.is_dir())
        .with_context(|| {
            format!(
                "no `share/` dir beside {} (expected the YoWASP wheel layout)",
                wasm.display()
            )
        })?;
    Ok((wasm, share))
}

/// Best-effort discovery of an installed `yowasp_yosys` package via Python.
fn discover_yowasp() -> Option<(PathBuf, PathBuf)> {
    for py in ["python3", "python"] {
        let out = std::process::Command::new(py)
            .args([
                "-c",
                "import yowasp_yosys, pathlib; \
                 p = pathlib.Path(yowasp_yosys.__file__).parent; \
                 print(p / 'yosys.wasm'); print(p / 'share')",
            ])
            .output()
            .ok()?;
        if out.status.success() {
            let text = String::from_utf8_lossy(&out.stdout);
            let mut lines = text.lines();
            let wasm = PathBuf::from(lines.next()?.trim());
            let share = PathBuf::from(lines.next()?.trim());
            if wasm.is_file() && share.is_dir() {
                return Some((wasm, share));
            }
        }
    }
    None
}

/// Compile `yosys.wasm` to native, caching the result on disk keyed by content
/// hash — mirrors `yowasp_runtime`'s serialize/deserialize cache so only the
/// first `build` pays the (large) cranelift compile.
fn load_module(engine: &Engine, wasm_path: &Path) -> Result<Module> {
    let bytes =
        std::fs::read(wasm_path).with_context(|| format!("reading {}", wasm_path.display()))?;

    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    // Distinguish incompatible serialized formats across wasmtime versions.
    env!("CARGO_PKG_VERSION").hash(&mut hasher);
    let key = format!("yosys-{:016x}.cwasm", hasher.finish());

    let cache_dir = cache_dir();
    let cache_file = cache_dir.join(&key);

    if cache_file.is_file() {
        // SAFETY: file is one we wrote with a matching engine/version key; any
        // mismatch is caught and we fall back to a fresh compile.
        if let Ok(m) = unsafe { Module::deserialize_file(engine, &cache_file) } {
            return Ok(m);
        }
    }

    clilog::info!("synth: compiling yosys.wasm (first run; cached after)");
    // cranelift/wasmtime log a torrent of DEBUG/TRACE through the `log` facade;
    // clip it to Info for the compile so the netlist output isn't buried, then
    // restore. Yosys's own stdout/stderr is inherited (not via `log`), so this
    // doesn't hide synthesis messages.
    let prior = log::max_level();
    if prior > log::LevelFilter::Info {
        log::set_max_level(log::LevelFilter::Info);
    }
    let module = Module::from_binary(engine, &bytes).context("compiling yosys.wasm");
    log::set_max_level(prior);
    let module = module?;
    if let Ok(serialized) = module.serialize() {
        let _ = std::fs::create_dir_all(&cache_dir);
        let _ = std::fs::write(&cache_file, serialized);
    }
    Ok(module)
}

/// Persistent cache dir: `$XDG_CACHE_HOME/jacquard` or `$HOME/.cache/jacquard`,
/// falling back to the system temp dir.
fn cache_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("jacquard");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home).join(".cache").join("jacquard");
        }
    }
    std::env::temp_dir().join("jacquard")
}

/// Run Yosys (`argv`) inside `work_dir` from the WASM module — a Rust port
/// of `yowasp_runtime.run_wasm`'s WASI setup (preopen cwd, `/share`, `/tmp`).
/// `argv[0]` is the program name; the rest are Yosys arguments.
fn run_yosys_wasm(
    wasm_path: &Path,
    share_dir: &Path,
    work_dir: &Path,
    argv: &[&str],
) -> Result<()> {
    let engine = Engine::default();
    let module = load_module(&engine, wasm_path)?;

    let mut linker: Linker<WasiP1Ctx> = Linker::new(&engine);
    p1::add_to_linker_sync(&mut linker, |t| t)?;

    let mut builder = WasiCtxBuilder::new();
    builder.inherit_stdin().inherit_stdout().inherit_stderr();
    builder.args(argv);
    // cwd -> work dir (relative reads/writes land here)
    builder.preopened_dir(work_dir, ".", DirPerms::all(), FilePerms::all())?;
    // Yosys's built-in datadir is compiled to /share in the wasi build.
    builder.preopened_dir(share_dir, "/share", DirPerms::all(), FilePerms::all())?;
    // Yosys (and in-process abc) want a writable /tmp.
    builder.preopened_dir(
        std::env::temp_dir(),
        "/tmp",
        DirPerms::all(),
        FilePerms::all(),
    )?;

    let wasi = builder.build_p1();
    let mut store = Store::new(&engine, wasi);
    let instance = linker.instantiate(&mut store, &module)?;
    let start = instance.get_typed_func::<(), ()>(&mut store, "_start")?;

    match start.call(&mut store, ()) {
        Ok(()) => Ok(()),
        Err(e) => match e.downcast_ref::<I32Exit>() {
            Some(exit) if exit.0 == 0 => Ok(()),
            Some(exit) => bail!("Yosys exited with code {}", exit.0),
            None => Err(e).context("Yosys WASM trap"),
        },
    }
}
