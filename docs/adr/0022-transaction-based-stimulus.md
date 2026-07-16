# ADR 0022 — Transaction-based external stimulus (SCE-MI-style pipes)

**Status:** Proposed

**Relates to:** [ADR 0013](0013-plural-peripheral-configs.md) (peripheral models,
GPU→CPU ring buffers), [ADR 0017](0017-cosim-execution-model.md) (batch dispatch),
[ADR 0021](0021-behavioral-rtl-support.md) (RTL on-ramp — what makes a
synthesizable transactor possible), and [Testbench Interop](../interop.md).

## Context

"Can I point my UVM / cocotb testbench at Jacquard?" is the most common question
we get, and [interop.md](../interop.md) currently answers it as an open one. It
isn't. The batching constant decides it.

### The number

`src/sim/cosim/mod.rs`:

```rust
/// Batch size for backend dispatch: number of consecutive scheduler edges run
/// in one `run_edges` call (no per-tick CPU interaction within a batch).
const BATCH_SIZE: usize = 1024;
```

**No per-tick CPU interaction within a batch.** That clause is where cosim's
throughput comes from: 1024 scheduler edges execute with the CPU absent. Device
timestamps put the run firmly GPU-compute-bound (the simulate kernel is ~72% of
per-edge GPU time; see [Cosim Perf Report](../cosim-perf-report.md)), which is
only true because nothing crosses the boundary in between.

A UVM driver is per-cycle by construction: it reaches through a virtual interface
and wiggles pins on every clock. Driving Jacquard that way forces `BATCH_SIZE`
to 1 and a CPU round-trip per edge, which deletes the reason to use a GPU. So
this was never an effort question. Any external stimulus that needs to observe
or drive **every cycle** is incompatible with the engine, and no amount of
implementation makes it otherwise.

### Why not run the testbench on the GPU

Running UVM *inside* Jacquard is not a long tail of missing features, it is a
category error:

- **No runtime to allocate into.** The AIG is elaborated once into a static
  instruction script (`FlattenedScriptV1`). UVM needs a heap, `new()`,
  inheritance, virtual dispatch, and a factory doing runtime type overrides.
- **No scheduler.** fork/join, mailboxes, events and UVM's phasing all sit on
  SystemVerilog's stratified event queue. Jacquard deliberately has no event
  queue — discarding it, and evaluating whole logic cones per edge, *is* the
  speed thesis.
- **No solver.** `randomize()` is a constraint solve per transaction, a search,
  not circuit evaluation.
- **No strings or associative arrays** for `uvm_config_db` and reporting.
- **Only immediate assertions.** `$check` lowers to `GEM_ASSERT` cells today;
  temporal SVA (`|->`, `##[1:$]`) needs automata with runtime state.

Building all that is writing a SystemVerilog simulator. And the payoff would be
negative: a testbench is sequential and dynamic, so it would run at CPU speed
*while* dragging the GPU into a per-edge handshake. A slow simulator inside a
fast one.

### The industry settled this

Emulators hit this wall decades ago and standardised the answer:
[SCE-MI](https://www.accellera.org/downloads/standards/sce-mi) (Accellera,
currently v2.4, Nov 2016). Split the testbench in two:

- an **untimed** side, on the host, holding sequences, randomisation, scoreboard
  and coverage;
- a **timed** side, in the emulator, holding a **transactor** (BFM) that turns
  messages into pin activity;
- **pipes** between them: buffered, flow-controlled, message-oriented channels
  with a configurable depth.

The property that matters for us: **a transactor may run ahead of the host, up
to the pipe's buffering**. That is the same statement as "no per-tick CPU
interaction within a batch", arrived at independently by people with the same
constraint. Jacquard being emulator-inspired (GEM) is not a coincidence here.

### No open-source testbench will drop into this

Worth stating plainly, because it sets expectations: **no open-source UVM
testbench uses SCE-MI pipes.** That isn't an oversight to be fixed, it's
structural — SCE-MI exists to talk to Palladium, Veloce and ZeBu, and
open-source UVM targets simulators, which have no boundary to amortise. The
public material is DVCon papers and the spec, not repositories.

The consequence for us: there is no ready-made UVM testbench to point at
Jacquard, and adopting this ADR will not produce one. **The transactor is ours
to write**, per protocol, and the untimed side has to be adapted to speak
transactions. Anyone hoping to lift an existing testbench off GitHub should read
that as a no.

### The open-source world solved it anyway, under another name

[FireSim](https://docs.fires.im/) / **Golden Gate (MIDAS II)** is the same
architecture with different vocabulary, and unlike SCE-MI it is readable code
rather than a PDF:

| SCE-MI | FireSim | here |
|---|---|---|
| transactor / BFM | **bridge** (target-side RTL + host-side software) | synthesizable BFM in the AIG |
| pipe | **token channel** | the CPU↔GPU rings |
| untimed / timed split | host-decoupling | CPU model / GPU batch |

Its central idea is worth more than the vocabulary: Golden Gate decomposes the
target into a dataflow graph of **latency-insensitive** models, so the target's
notion of time is decoupled from the host's and either side may run ahead or
stall without changing the result. That is the general form of what
`BATCH_SIZE` does for us by hand, and it is the reason FireSim can be both
FPGA-accelerated and deterministic.

We cannot lift the implementation — FireSim is Chisel/FIRRTL onto FPGAs, we are
Verilog/AIG onto GPUs — but it is the closest prior art with source, and the
place to look before inventing channel semantics.

### Two things recently made this reachable

1. **[ADR 0021](0021-behavioral-rtl-support.md) shipped** (v0.3.0). A transactor
   is synthesizable RTL, so it now compiles into the same AIG as the DUT through
   the on-ramp. Before that, every protocol meant a hand-written kernel.
2. **Half the channel exists.** [ADR 0013](0013-plural-peripheral-configs.md)
   already defines GPU→CPU ring buffers, carrying UART bytes and bus traces out
   of a batch.

## Decision

Adopt the SCE-MI **split and pipe semantics** as the model for external
stimulus. Do not implement the SCE-MI API.

- **Untimed side (host).** Sequences, randomisation, checking. UVM in a real
  simulator, or cocotb, or plain Rust — the model doesn't care.
- **Timed side (GPU).** A transactor written as synthesizable SystemVerilog,
  compiled via the on-ramp into the AIG beside the DUT, so it lives *inside* the
  batch and wiggles pins at GPU speed.
- **Between them: pipes.** Buffered, flow-controlled, message-oriented, with a
  depth. Semantics only — the C API, the SCE-MI 1 macro-based message ports, and
  DMI are all out of scope.

What this requires, concretely:

| Direction | Status |
|---|---|
| GPU→CPU (responses, monitors) | **exists** — the ADR 0013 ring buffers |
| CPU→GPU (transactions in) | **missing** — the one new mechanism |

The CPU→GPU pipe must be **pre-loaded before dispatch, not streamed**. `ulib`'s
H2D is synchronous, so uploading per edge reintroduces exactly the stall the
batch exists to avoid. Load a batch's worth of messages into device memory, and
let a GPU-side feeder pop them as the transactor asserts ready.

### The sizing rule

**Pipe depth buys batch depth.** A transfer of ~4–40 cycles means a 1024-edge
batch consumes roughly 25–250 transactions, so the pipe must hold at least that
or the batch stalls on an empty queue and we are back to per-edge. This is the
design's one hard number, and the metric to judge it by is **transactions
retired per batch**, not features supported.

## Consequences

- **Existing UVM drivers do not port.** Their timed half is rewritten as a BFM.
  That is the standard SCE-MI/TBX workflow rather than a Jacquard tax, but it is
  real work and should be stated plainly to anyone asking.
- **Suits protocol transactors, not big memories.** A BFM has little state. A
  16 MB flash backing store does not want to be an AIG memory, so bespoke
  kernels keep earning their place there.
- **Gives ADR 0013's target architecture a second answer.** Rather than
  generalising the bespoke-kernel pattern to every peripheral, some peripherals
  could simply *be RTL*.
- **UVM is not free at the end of this.** A host adapter (DPI or a socket) is
  still needed to put a real SystemVerilog simulator on the untimed side.
- **The failure mode is a starved pipe.** If the host cannot keep it full, the
  batch stalls and the GPU idles — the same cliff, reached from the other side.

## Alternatives considered

- **Per-cycle bridge (naive cocotb/DPI).** Rejected: `BATCH_SIZE` → 1. This is
  what [interop.md](../interop.md) means by "a naive bridge would marshal
  Python ↔ GPU every cycle"; the number above is the reason.
- **UVM runtime on the GPU.** Rejected, see Context — an SV simulator, and
  slower than the thing it replaces.
- **Record-and-replay only.** Works today and stays the recommended interim
  path, but it is not reactive: the stimulus cannot depend on the design.
- **Vendoring the SCE-MI spec into `docs/`.** Rejected: it is Accellera's
  copyrighted work, a checked-in copy has no update path, and we need the split
  and the pipes, not the DPI binding details. Cite it, don't copy it.

## References

- SCE-MI (Standard Co-Emulation Modeling Interface), Accellera —
  [standard downloads](https://www.accellera.org/downloads/standards/sce-mi).
  v2.4 (Nov 2016) is current at the time of writing.
- FireSim / Golden Gate (MIDAS II), UC Berkeley — the same split in open source,
  with code: [target-to-host bridges](https://docs.fires.im/en/latest/Golden-Gate/Bridges.html)
  (transactors), [target abstraction & host decoupling](https://docs.fires.im/en/latest/Golden-Gate/LI-BDN.html)
  (latency-insensitive models and token channels). Read before designing the
  channel semantics.
- [Testbench Interop](../interop.md) — what Jacquard drives today, and the
  record-and-replay fallback.
- [Cosim Perf Report](../cosim-perf-report.md) — where the per-edge time goes,
  and how the GPU-bound claim was measured.
