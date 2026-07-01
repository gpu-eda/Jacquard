# Spike — `jacquard build` via Rust + `wasmtime` (no Python)

Throwaway proof backing [ADR 0021](../../adr/0021-behavioral-rtl-support.md):
can a Rust process drive YoWASP's stock `yosys.wasm` to run the aigpdk
synthesis flow, with **no Python interpreter and no external toolchain**?

**Result: yes, end-to-end.** This directory is preserved as an implementation
reference; the real feature is `jacquard build` (see
[#162](https://github.com/gpu-eda/Jacquard/issues/162)).

## What it does

`src/main.rs` is a ~55-line faithful port of `yowasp_runtime.run_wasm`'s WASI
setup into the `wasmtime` Rust crate: load `yosys.wasm`, preopen the design dir
(cwd), the YoWASP `share/` dir as `/share`, and a temp dir as `/tmp`, set argv
`["yosys", "-s", "synth.ys"]`, run `_start`, and handle the WASI exit trap.

`work/synth.ys` runs the aigpdk logic-synthesis flow from
[`docs/synthesis-flow.md`](../../synthesis-flow.md) Step 2 (Yosys path) against
`work/counter.v` (a trivial 8-bit synchronous counter).

## Run

```sh
# yosys.wasm + share/ come from the resolved yowasp-yosys wheel (uv.lock);
# locate them under the uv cache, e.g.:
#   python -c "import yowasp_yosys, pathlib; print(pathlib.Path(yowasp_yosys.__file__).parent)"
cargo run --release -- <yosys.wasm> <yowasp-share-dir> "$PWD/work"
```

## What the spike proved

1. **No Python needed** — the wasmtime Rust crate runs the stock wheel's
   `yosys.wasm` unmodified.
2. **The aigpdk flow runs in WASM** — `read_verilog → synth → dfflibmap → abc
   → techmap → abc → write_verilog`, producing `work/gatelevel.gv` mapped
   entirely to aigpdk cells (`AND2_*`, `INV`, `DFF`), zero leftover `$` cells.
3. **In-process abc works under WASI** — the ABC pass ran to completion with no
   `exec`. This directly de-risks the Phase-2 provenance toolchain: the
   remaining unknown is only whether `&origins`/`\src` *data* survives the
   in-process abc path, not whether in-process abc functions at all.

## Not covered here (real `build` work, #162)

- Assertion lowering to `GEM_ASSERT` + default `read_verilog -sv` (so immediate
  assertions/`$finish` survive — see ADR 0021 / `docs/input-netlist.md`).
- Memory synthesis (`memlib_yosys.txt`) for designs with RAM.
- wasm asset sourcing (bundle-in-binary vs fetch-to-cache).
- `--top-module`, clock-gating config, back-end abstraction (YoWASP/Nix/system).
