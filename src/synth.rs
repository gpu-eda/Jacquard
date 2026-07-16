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
    /// Explicit path to `yosys.wasm`; else `JACQUARD_YOSYS_WASM` / the pinned fetch.
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

    let cache_key = synth_cache_key(
        &design_bytes,
        &script,
        &wasm_bytes,
        // Every file the script reads out of the work dir. Add here when the run
        // gains an input, or editing that input yields a stale hit.
        &[AIGPDK_NOMEM_LIB, MEMLIB_YOSYS, GEM_FORMAL_V],
    );
    let cache_dir = cache_dir();
    let cached = cache_dir.join(&cache_key);

    // Escape hatch for bisecting a synthesis problem, where a hit is exactly
    // what you don't want. Still refreshes the entry afterwards.
    let bypass = std::env::var_os("JACQUARD_NO_SYNTH_CACHE").is_some_and(|v| !v.is_empty());

    if cached.is_file() && !bypass {
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

        // `-l`: Yosys tees its log to the file as well as stdout, so a crash can
        // be reported with the pass that died (see `describe_wasm_trap`). Stdout
        // is still inherited, so the user's view is unchanged.
        run_yosys_wasm(
            &wasm_path,
            &share_dir,
            wp,
            &["yosys", "-l", "synth.log", "-s", "synth.ys"],
        )
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

/// Content hash of *everything* that determines the synthesized netlist.
///
/// Every input the run reads has to be in here, or a change to it yields a
/// stale hit — the netlist gets reused despite having been produced under
/// different conditions. The non-obvious members are the three embedded support
/// files: they're `include_str!`, so editing `aigpdk/aigpdk_nomem.lib` changes
/// the binary but *not* `CARGO_PKG_VERSION`, which only moves at release. Before
/// they were hashed, editing the standard-cell library the script maps against
/// (`read_liberty -lib aigpdk_nomem.lib`) silently reused a netlist built with
/// the old one.
///
/// `script` covers the top module, assertion handling, and the chosen SV
/// frontend, since all three are baked into the text. `CARGO_PKG_VERSION` stays
/// as a coarse backstop for engine changes the inputs above don't capture.
///
/// `support` is taken as an argument rather than read from the consts directly
/// so the tests can vary it: hashing a `const` can't be exercised by a test that
/// can't change it, and a test that cannot fail is worse than none.
///
/// If you add an input to the synthesis run, add it to `support` at the call
/// site — `cache_key_covers_the_support_files` makes dropping one loud.
fn synth_cache_key(
    design_bytes: &[u8],
    script: &str,
    wasm_bytes: &[u8],
    support: &[&str],
) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    design_bytes.hash(&mut h);
    script.hash(&mut h);
    wasm_bytes.hash(&mut h);
    for s in support {
        s.hash(&mut h);
    }
    env!("CARGO_PKG_VERSION").hash(&mut h);
    format!("synth-{:016x}.gv", h.finish())
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
# Load the aigpdk cells as blackbox modules up front so abc9's BOX_DERIVE can
# build proper sequential (flop) boxes for abc_new's XAIGER path (WS-A). Without
# this, abc_new fails to wire flop clocks (Bad connection .../CLK ~ clk).
read_liberty -lib aigpdk_nomem.lib
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
# Purge unused blackbox lib cells (e.g. BUF) before abc_new; otherwise abc9
# errors \"Module '...' with (* abc9_box *) has no timing information\".
hierarchy{top_arg} -purge_lib
# Map with abc_new (abc9/aiger2 XAIGER path) so Yosys (* src *) origins ride the
# in-memory AIGER y-channel through mapping (ADR 0021 Phase 2 / WS-A). Classic
# abc -liberty routes through BLIF, which has no object identity and drops all
# provenance. origins_max bounds the number of origins tracked per node.
# A single abc9 pass fully maps to aigpdk cells (dch,-f recovers QoR vs bare &nf);
# the classic two-pass (abc; techmap; abc) dance breaks abc_new's XAIGER path.
scratchpad -set abc9.origins_max 100
abc_new -script +&dch,-f;&nf -liberty aigpdk_nomem.lib
opt_clean -purge

# Keep attributes: write_verilog -noattr would strip \\src (the WS-A goal).
write_verilog gatelevel.gv
stat
"
    )
}

// Pinned provenance-carrying `yosys.wasm` (ADR 0021 Phase 2 / A2). Built,
// validated (comb + seq2 `\src` coverage), and released by `gpu-eda/yowasp-yosys`'s
// `provenance-wasm` CI — a fork of YoWASP Yosys carrying forked yosys + abc
// `&origins` + yosys-slang. Its `release` job attaches the wheel to a
// gpu-eda/yowasp-yosys release (in-org, public), which this fetch pins. The
// abc_new origins flow the on-ramp uses REQUIRES this fork — stock `abc_new`
// breaks on flops (`ff.cc: Bad connection .../CLK`) — so this is the default wasm
// source rather than a discovered stock YoWASP wheel. Re-pin all three together
// from the release the CI publishes (its sha256 is in the release body).
const YOSYS_WASM_TAG: &str = "wasm-de797e07";
const YOSYS_WASM_URL: &str = "https://github.com/gpu-eda/yowasp-yosys/releases/download/\
wasm-de797e07/yowasp_yosys-0.63.0.0.post1136-py3-none-any.whl";
const YOSYS_WASM_SHA256: &str = "15726fed2b85eb8c55a50bbacb68c5812d270cbcf46bea310d23351d320ef007";

/// Resolve `yosys.wasm` and its `share/` dir: explicit arg → `JACQUARD_YOSYS_WASM`
/// env → the pinned provenance wasm (fetched + cached on first use, A2). The old
/// "discover any installed `yowasp_yosys`" fallback is gone: the abc_new origins
/// flow requires the fork wasm, and a stock discovered wheel would silently break
/// on sequential logic. Point `--yosys-wasm`/`JACQUARD_YOSYS_WASM` at your own
/// fork build to work offline.
fn locate_yosys_wasm(explicit: Option<&Path>) -> Result<(PathBuf, PathBuf)> {
    if let Some(p) = explicit {
        return with_share(p.to_path_buf());
    }
    if let Ok(env) = std::env::var("JACQUARD_YOSYS_WASM") {
        if !env.is_empty() {
            return with_share(PathBuf::from(env));
        }
    }
    fetch_pinned_wasm()
}

/// Fetch, verify (sha256), and unpack the pinned provenance `yosys.wasm` into the
/// cache, returning `(wasm, share)`. Downloads only on first use; later runs reuse
/// the unpacked dir. The asset is the YoWASP wheel (a zip) laying out
/// `yowasp_yosys/yosys.wasm` + `yowasp_yosys/share/`.
fn fetch_pinned_wasm() -> Result<(PathBuf, PathBuf)> {
    let base = cache_dir().join("wasm").join(YOSYS_WASM_TAG);
    let wasm = base.join("yowasp_yosys").join("yosys.wasm");
    // Cache hit: already unpacked and structurally valid (wasm + sibling share).
    if let Ok(pair) = with_share(wasm.clone()) {
        return Ok(pair);
    }

    clilog::info!(
        "synth: fetching pinned provenance yosys.wasm ({YOSYS_WASM_TAG}) — first run only"
    );
    let bytes = http_get(YOSYS_WASM_URL)
        .with_context(|| format!("downloading pinned yosys.wasm from {YOSYS_WASM_URL}"))?;
    verify_sha256(&bytes, YOSYS_WASM_SHA256)?;

    // Unpack into a sibling temp dir, then atomically rename into place so a
    // concurrent/interrupted run never sees a half-written cache.
    let parent = base.parent().expect("cache/wasm has a parent");
    std::fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(".{YOSYS_WASM_TAG}.tmp"));
    let _ = std::fs::remove_dir_all(&tmp);
    unzip(&bytes, &tmp).context("unpacking YoWASP wheel")?;
    with_share(tmp.join("yowasp_yosys").join("yosys.wasm"))
        .context("fetched wheel is missing yowasp_yosys/{yosys.wasm,share} — pin may be wrong")?;
    let _ = std::fs::remove_dir_all(&base);
    std::fs::rename(&tmp, &base)
        .with_context(|| format!("installing yosys.wasm to {}", base.display()))?;
    clilog::info!("synth: cached provenance yosys.wasm at {}", wasm.display());
    with_share(wasm)
}

/// Blocking HTTP GET returning the full body (the wheel is ~12 MB). GitHub release
/// download URLs redirect to a signed objects host; `ureq` follows them.
fn http_get(url: &str) -> Result<Vec<u8>> {
    use std::io::Read;
    let resp = ureq::get(url)
        .call()
        .map_err(|e| anyhow::anyhow!("HTTP GET failed: {e}"))?;
    let mut buf = Vec::new();
    resp.into_reader()
        .read_to_end(&mut buf)
        .context("reading HTTP response body")?;
    Ok(buf)
}

/// Verify `bytes` hash to `expected` (lower-hex sha256); error otherwise.
fn verify_sha256(bytes: &[u8], expected: &str) -> Result<()> {
    use sha2::{Digest, Sha256};
    let got = format!("{:x}", Sha256::digest(bytes));
    if got != expected {
        bail!("yosys.wasm sha256 mismatch: expected {expected}, got {got}");
    }
    Ok(())
}

/// Extract a zip archive (the YoWASP wheel) held in memory into `dest`.
fn unzip(bytes: &[u8], dest: &Path) -> Result<()> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i)?;
        let Some(rel) = f.enclosed_name() else {
            continue; // skip entries with unsafe (`..`) paths
        };
        let out = dest.join(rel);
        if f.is_dir() {
            std::fs::create_dir_all(&out)?;
        } else {
            if let Some(p) = out.parent() {
                std::fs::create_dir_all(p)?;
            }
            let mut w = std::fs::File::create(&out)?;
            std::io::copy(&mut f, &mut w)?;
        }
    }
    Ok(())
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
            // A trap is the engine dying — abort(), a failed assert, or memory
            // unsafety — not an ordinary error. Yosys reports those itself and
            // exits non-zero, which lands in the arm above. See gpu-eda/Jacquard#211.
            None => bail!(describe_wasm_trap(&e, work_dir)),
        },
    }
}

/// Turn a wasm trap into something a person can act on.
///
/// `wasmtime`'s Display for a trap appends the whole wasm backtrace: dozens of
/// `<wasm function 32210>` frames that are meaningless without the module's
/// symbols, and which bury the one fact that matters — synthesis crashed, and
/// where. Keep the trap kind, drop the frames, and name the last pass Yosys
/// announced (from the `-l` log) so the report points at a culprit.
fn describe_wasm_trap(e: &anyhow::Error, work_dir: &Path) -> String {
    // Trap's Display already reads "wasm trap: …", so don't prefix it again.
    let kind = e
        .downcast_ref::<wasmtime::Trap>()
        .map(|t| t.to_string())
        .unwrap_or_else(|| "wasm trap".to_string());

    let last_pass = std::fs::read_to_string(work_dir.join("synth.log"))
        .ok()
        .and_then(|log| {
            log.lines()
                .filter(|l| l.contains(". Executing "))
                .next_back()
                .map(|l| l.trim().to_string())
        });

    let mut msg = String::from("the synthesis engine crashed (");
    msg.push_str(&kind);
    msg.push(')');
    match last_pass {
        Some(pass) => {
            msg.push_str("\n  it died in: ");
            msg.push_str(&pass);
        }
        None => msg.push_str("\n  (no Yosys log was captured; see the output above)"),
    }
    msg.push_str(
        "\n  A crash is an engine bug rather than a problem with your RTL, though \
         unusual input can trigger one.\n  Please report it, with the design, at \
         https://github.com/gpu-eda/Jacquard/issues",
    );
    msg
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUP: &[&str] = &[AIGPDK_NOMEM_LIB, MEMLIB_YOSYS, GEM_FORMAL_V];

    /// Every input that changes the netlist must change the key. A miss here is
    /// a stale cache hit in the field: the user silently gets a netlist built
    /// under conditions that no longer hold.
    #[test]
    fn cache_key_covers_each_direct_input() {
        let base = synth_cache_key(
            b"module m(); endmodule",
            "read_verilog m.v\n",
            b"\0asm-a",
            SUP,
        );

        assert_ne!(
            base,
            synth_cache_key(
                b"module n(); endmodule",
                "read_verilog m.v\n",
                b"\0asm-a",
                SUP
            ),
            "a different design must not reuse the netlist"
        );
        assert_ne!(
            base,
            synth_cache_key(
                b"module m(); endmodule",
                "read_slang m.v\n",
                b"\0asm-a",
                SUP
            ),
            "a different script (top module, assertions, SV frontend) must not reuse it"
        );
        assert_ne!(
            base,
            synth_cache_key(
                b"module m(); endmodule",
                "read_verilog m.v\n",
                b"\0asm-b",
                SUP
            ),
            "a different yosys.wasm must not reuse it"
        );
    }

    /// The support files are synthesis inputs: the script does `read_liberty
    /// -lib aigpdk_nomem.lib`, `memory_libmap -lib memlib_yosys.txt`, `techmap
    /// -map gem_formal.v`. They arrive by `include_str!`, so editing one moves
    /// neither the design, the script, the wasm, nor `CARGO_PKG_VERSION` (which
    /// only moves at release) — they were absent from the key, and editing the
    /// standard-cell library silently reused a netlist mapped against the old
    /// one. Verified by hand before the fix: same key, "cache hit", stale
    /// netlist.
    #[test]
    fn cache_key_covers_the_support_files() {
        let k = |support: &[&str]| synth_cache_key(b"d", "s", b"w", support);
        let real = k(SUP);

        assert_ne!(
            real,
            k(&["edited", MEMLIB_YOSYS, GEM_FORMAL_V]),
            "editing the liberty the script maps against must change the key"
        );
        assert_ne!(
            real,
            k(&[AIGPDK_NOMEM_LIB, "edited", GEM_FORMAL_V]),
            "editing the memory-mapping rules must change the key"
        );
        assert_ne!(
            real,
            k(&[AIGPDK_NOMEM_LIB, MEMLIB_YOSYS, "edited"]),
            "editing the assertion techmap must change the key"
        );
        assert_ne!(
            real,
            k(&[AIGPDK_NOMEM_LIB, MEMLIB_YOSYS]),
            "dropping a support file from the key is exactly the bug that shipped"
        );
    }

    /// Keys are filenames; a stray path separator would escape the cache dir.
    #[test]
    fn cache_key_is_a_plain_filename() {
        let k = synth_cache_key(b"d", "s", b"w", SUP);
        assert!(k.starts_with("synth-") && k.ends_with(".gv"), "{k}");
        assert!(!k.contains('/') && !k.contains(".."), "{k}");
    }

    /// Guard the pinned-wasm trio against a copy-paste slip on re-pin: the
    /// download URL must reference the tag, and the hash must be a 64-char
    /// lower-hex sha256. The publish workflow greps the same consts, so keeping
    /// them internally consistent keeps the workflow and the runtime in sync.
    #[test]
    fn pinned_wasm_consts_consistent() {
        assert!(
            YOSYS_WASM_URL.contains(YOSYS_WASM_TAG),
            "YOSYS_WASM_URL must reference YOSYS_WASM_TAG (bump both together)"
        );
        assert_eq!(YOSYS_WASM_SHA256.len(), 64, "sha256 is 64 hex chars");
        assert!(
            YOSYS_WASM_SHA256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "sha256 must be lower-hex"
        );
    }
}
