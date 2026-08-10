// Spike: run YoWASP's stock `yosys.wasm` from Rust via wasmtime, driving the
// aigpdk synthesis flow. Proves the on-ramp needs no Python — a faithful port
// of yowasp_runtime.run_wasm's WASI setup into the wasmtime Rust crate.

use anyhow::{Context, Result};
use wasmtime::{Engine, Linker, Module, Store};
use wasmtime_wasi::p1::{self, WasiP1Ctx};
use wasmtime_wasi::{DirPerms, FilePerms, I32Exit, WasiCtxBuilder};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let wasm_path = &args[1]; // path to yosys.wasm
    let share_dir = &args[2]; // yowasp share/ dir -> guest /share
    let work_dir = &args[3]; // design dir (cwd, holds counter.v / synth.ys)

    let engine = Engine::default();
    println!("[spike] compiling {wasm_path} ...");
    let module = Module::from_file(&engine, wasm_path)
        .with_context(|| format!("loading {wasm_path}"))?;

    let mut linker: Linker<WasiP1Ctx> = Linker::new(&engine);
    p1::add_to_linker_sync(&mut linker, |t| t)?;

    let mut builder = WasiCtxBuilder::new();
    builder.inherit_stdin().inherit_stdout().inherit_stderr();
    // argv[0] is the program name; then the yosys CLI args.
    builder.args(&["yosys", "-s", "synth.ys"]);
    // cwd -> the design/work dir (relative reads+writes land here)
    builder.preopened_dir(work_dir, ".", DirPerms::all(), FilePerms::all())?;
    // yosys's built-in data dir is compiled to /share in the wasi build
    builder.preopened_dir(share_dir, "/share", DirPerms::all(), FilePerms::all())?;
    // yosys wants a writable /tmp
    let tmp = std::env::temp_dir();
    builder.preopened_dir(tmp, "/tmp", DirPerms::all(), FilePerms::all())?;

    let wasi = builder.build_p1();
    let mut store = Store::new(&engine, wasi);

    let instance = linker.instantiate(&mut store, &module)?;
    let start = instance.get_typed_func::<(), ()>(&mut store, "_start")?;

    println!("[spike] running yosys -s synth.ys ...");
    match start.call(&mut store, ()) {
        Ok(()) => {
            println!("[spike] yosys returned (no explicit exit)");
            Ok(())
        }
        Err(e) => {
            if let Some(exit) = e.downcast_ref::<I32Exit>() {
                let code = exit.0;
                println!("[spike] yosys exited with code {code}");
                std::process::exit(code);
            }
            Err(e)
        }
    }
}
