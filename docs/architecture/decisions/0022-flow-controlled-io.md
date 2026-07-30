# Decision 0022 — Flow-controlled external I/O across the batch boundary

**Status:** Proposed

**Relates to:** [Decision 0013](0013-plural-peripheral-configs.md) (peripheral models and the GPU→CPU ring buffers, which are half of this channel already), [Decision 0017](0017-cosim-execution-model.md) (batch dispatch, where `BATCH_SIZE` lives), [Decision 0021](0021-behavioral-rtl-support.md) (the RTL on-ramp that makes a synthesizable transactor possible), and [Testbench Interop](../../interop.md).

## Implementation status

Proposed; none of this is built. The code today has a fixed `BATCH_SIZE = 1024` ([Decision 0017](0017-cosim-execution-model.md)), the GPU→CPU ring buffers from [Decision 0013](0013-plural-peripheral-configs.md), and the per-edge full readback the cost section describes. What this ADR adds, the adaptive window, per-tap GPU-side projection, the CPU→GPU input pipe, the reactor, and any SCE-MI adapter, is unbuilt. The design sections below are written in the present tense as the intended shape, not current behaviour.

## The batch is where the speed comes from

Everything external to the simulation crosses one seam: the batch. A testbench driving stimulus in, a video renderer streaming samples out, a debugger reaching in and reading back are all I/O across the same GPU↔CPU boundary, and one constant governs every one of them.

```rust
/// Batch size for backend dispatch: number of consecutive scheduler edges run
/// in one `run_edges` call (no per-tick CPU interaction within a batch).
const BATCH_SIZE: usize = 1024;
```

The parenthetical is the whole story. 1024 scheduler edges execute with the CPU absent, which is why cosim is GPU-compute-bound (the simulate kernel is about 72% of per-edge GPU time; see the [Cosim Perf Report](../../cosim-perf-report.md)). Nothing crosses the boundary in between, and that is the only reason the number holds. Any external I/O that reaches across inside a batch pulls the boundary back to per-edge and deletes the point of using a GPU.

A UVM driver does exactly that. It reaches through a virtual interface and wiggles pins on every clock, which forces `BATCH_SIZE` to 1 and a CPU round-trip per edge. So the question was never one of effort. Anything that has to observe or drive every cycle is incompatible with the engine, and no implementation makes it otherwise.

The bound is per-cycle specifically, not latency in general, and the difference is easy to miss. A consumer that wants low latency at a *domain* cadence (a video tap at vsync, roughly 16 ms; a link at a packet boundary) crosses on that design signal rather than every clock. That cadence is coarse, so the batch stays large and the GPU stays busy. Latency-sensitive I/O and deep batches coexist; only per-cycle interaction collapses the batch to 1.

## The cost is the readback, not the socket

The signal-streaming and video tap (#184) measured about half its wall-clock on the CPU side. The natural guess was the socket, but the socket write is already non-blocking and drops on backpressure, so that isn't where the time went. It goes to the full per-edge state readback the tap rides on. Moving *state* across the boundary is the expensive part, not the syscall, so the fix is a correctly shaped and sized pipe rather than a faster socket. That is what this ADR is about.

## Cross the boundary through bounded pipes

The decision that both applications below build on: cross the batch boundary only through bounded, flow-controlled FIFOs, sized so either side can run ahead of the other up to the pipe's depth without changing the result. This is latency-insensitive I/O, and it is settled prior art rather than an invention.

Emulators standardised the answer decades ago. [SCE-MI](https://www.accellera.org/downloads/standards/sce-mi) (Accellera, v2.4, 2016) splits the testbench into an untimed host side and a timed emulator side, with buffered, flow-controlled, message-oriented pipes of configurable depth between them. The property that matters is that a transactor may run ahead of the host up to the pipe's buffering, which is the same statement as "no per-tick CPU interaction within a batch" reached independently under the same constraint. Jacquard being emulator-inspired (GEM) isn't a coincidence.

[FireSim / Golden Gate (MIDAS II)](https://docs.fires.im/) is the same architecture in open source, as readable code rather than a PDF. The vocabulary maps cleanly:

| SCE-MI | FireSim | here |
|---|---|---|
| transactor / BFM | bridge (target RTL + host software) | synthesizable BFM in the AIG, or a CPU-side tap |
| pipe | token channel | the CPU↔GPU rings |
| untimed / timed split | host-decoupling | CPU model / GPU batch |

Golden Gate's idea outlasts its vocabulary. It decomposes the target into a dataflow graph of latency-insensitive models, so either side can run ahead or stall without changing the result. That is the general form of what `BATCH_SIZE` does by hand, and why FireSim is both accelerated and deterministic. We can't lift the implementation (Chisel/FIRRTL onto FPGAs against Verilog/AIG onto GPUs), but it is the closest prior art with source, and the place to look before inventing channel semantics.

## The window is the only knob

One value controls everything: the commit window, meaning how many edges the GPU runs before it lets the CPU drain. At the GPU boundary the buffer *is* the window. The upstream (GPU→CPU) ring holds one batch's snapshots, so its depth is the window. The input (CPU→GPU) pipe is pre-loaded to feed the window, so its depth is `window / crossing-rate`. Nobody sets those depths; you size the window and they fall out. Today's fixed `BATCH_SIZE = 1024` is the degenerate case this replaces, a fixed window and therefore a fixed depth, wrong at both ends: too small for bulk throughput, too large for interactive latency.

The stall is still a real risk, and it is the same cliff from either side. If the window outruns what the pipes can hold, the batch stalls and the GPU idles on a starved input or a full output ring. But since the depths derive from the window, that can't come from mis-sizing a buffer. It comes only from pushing the window past a genuine limit, and three limits bound it. Memory is one, because a deeper window needs a bigger ring and device memory is the ceiling; this is why per-tap projection matters, since small entries make deep windows affordable. Latency is the second, because a big window delays a latency-sensitive consumer, so a latency budget or a flush signal caps it. Input supply is the third, because an input-gating pipe fills only as fast as the host produces, and if it starves the window shrinks on its own.

So the caller never picks a depth. It declares intent (a latency budget or a flush signal, and a backpressure policy), and the window adapts within those limits with every depth derived. The one buffer that isn't window-derived is the downstream CPU→socket ring, the slack between draining the GPU ring and writing the socket, and even there the knob is the policy rather than a size: `DropOldest` for video (a generous default, overflow drops) or `Block` for lossless JTAG. A caller can pin a depth as an optional override when the workload is known and warm-up should be skipped, but that is the exception, not the contract.

Our vocabulary diverges from SCE-MI's here, and it is worth being precise. A settable depth is SCE-MI's *FIFO* semantics, not its *pipe* depth, which is query-only (§5.8.5.7). That settable case surfaces here only at the optional override and the socket buffer. Most depth isn't set at all; it is the window.

Three levers realise the adaptive window, in rising order of sophistication. The first is mode selection: bound the window by the tightest attached consumer's class (small for interactive, large for bulk) plus a backpressure clamp. It is predictable and the right first move. The second is the flush signal, a designated design signal that closes the window when it fires, vsync for a video tap or a packet boundary for a link. The window becomes "until the next domain event", which stays large in edge terms so GPU throughput is untouched, while delivery tracks the domain's own cadence. This is the concrete form of the per-cycle-versus-domain-cadence distinction from earlier, and SCE-MI pipes already carry it as `eom` end-of-message markers and auto-flush (§5.8), prior art to borrow rather than reinvent. The third, for when a tight consumer and a bulk consumer share a run, decouples the GPU commit window (kept large) from per-consumer drain cadence by having the GPU publish a progress counter the drain polls. That costs mid-batch GPU→CPU visibility (a device-written atomic plus a CPU poll, cheap on Metal and managed memory but worth confirming per backend), and it is deferred until the flush signal proves insufficient. Continuous AIMD control is deferred further still.

Judge any of it by crossings retired per batch (transactions in, frames out), not by features supported.

## Backpressure and the input/output asymmetry

Backpressure policy belongs to each pipe, not to the system, because the two directions want opposite things. A lossless input blocks: a dropped transaction or bitbang command corrupts the session, so the pipe is sized and the producer waits when it fills. A streaming output drops: latest-wins, never stall the sim, since a late frame is worth less than a stalled GPU.

The deeper asymmetry is that outputs decouple and inputs don't. An output tap is fire-and-forget into a ring, and the sim never waits on it. An input-driving pipe is different in kind, because the sim's next step depends on the message. Flow control makes the I/O asynchronous, with no syscall on the hot path, but it can't remove that dependency, so when a gating input's pipe is empty and the sim needs the next message, it stalls. That isn't a bug to engineer away; it is what interactive debug is, and it is why the JTAG-server run sits at a large edge ceiling with the sim as the fast side waiting on OpenOCD.

## Determinism is the line

Cosim is byte-identical across CpuBackend, CUDA, HIP and Metal, and that equivalence is the correctness oracle. The pipes can fill and drain whenever they like, but the sim's consumption and sampling points stay deterministic, tied to sim time and FIFO order (TCP preserves order) and never to wall-clock. Windowing changes where we sync, never what the sim computes. Replay and bulk capture stay bit-exact regardless of window size or when a socket was serviced; only genuinely-live interactive sessions run on wall-clock time, and those were never golden-compared. The moment a message's wall-clock arrival could change which edge it lands on, the equivalence breaks. That is the line the design can't cross.

## Driving stimulus in: transactors, not testbenches

The input case is pointing a UVM, cocotb or Rust testbench at Jacquard, and the model is SCE-MI's split. The untimed side (sequences, randomisation, checking) runs on the host, as UVM in a real simulator, cocotb, or plain Rust; the model doesn't care which. The timed side is a transactor written as synthesizable SystemVerilog, compiled through the on-ramp ([Decision 0021](0021-behavioral-rtl-support.md)) into the AIG next to the DUT, so it lives inside the batch and wiggles pins at GPU speed. Pipes sit between them, as above.

How the transactor attaches is the part that shows the architecture already fits, and it corrects a mistake worth stating plainly: there is no DPI running on the emulator fabric, and there wouldn't be on ours. A SCE-MI transactor is two pieces, split exactly where our design puts a FIFO boundary. Its synthesizable half touches only a structural pipe endpoint, an interface exposing ready/valid/message wires (`scemi_input_pipe` and `scemi_output_pipe`), which the on-ramp synthesises like any RTL with the endpoint as a blackbox boundary cell. The DPI and the `scemi_pipe_c_*` calls are host-side glue that attaches to that endpoint and marshals to the untimed side. The synthesised transactor never calls C; it drives structural wires, and the host runtime supplies the driver.

That is why SCE-MI is a plausible future interface rather than something the design forecloses. We aren't implementing the SCE-MI API now, but our pipe is the same FIFO-at-a-boundary plus host-driver split that SCE-MI needs, so a conformant front-end could slot on later as an adapter over the same rings. Leaving it unbuilt for now is a deliberate choice, and the design keeps it reachable.

The one mechanism we're missing is the input pipe. The GPU→CPU direction (responses, monitors) already exists as the Decision 0013 ring buffers; the CPU→GPU direction (transactions in) doesn't. It has to be pre-loaded before dispatch rather than streamed, because `ulib`'s H2D is synchronous and uploading per edge reintroduces the very stall the batch avoids. Load a batch's worth of messages into device memory and let a GPU-side feeder pop them as the transactor asserts ready.

Running the untimed side on the GPU instead is a category error, not a long tail of missing features. The AIG is elaborated once into a static script (`FlattenedScriptV1`), so there is no heap for UVM's `new()` and factory, no event queue for fork/join and phasing (throwing that queue away is the speed thesis), no solver for `randomize()`, no strings or associative arrays for `uvm_config_db`, and only immediate assertions (temporal SVA needs runtime automata). Building all that is writing a SystemVerilog simulator, and it would run at CPU speed while dragging the GPU into a per-edge handshake, a slow simulator running inside a fast one.

The reason to adopt SCE-MI's semantics but not chase its API is the ecosystem, or the lack of one. SCE-MI exists to talk to Palladium, Veloce and ZeBu; open-source UVM targets simulators, which have no boundary to amortise, so no public UVM testbench uses SCE-MI pipes. A survey of the field turned up exactly one open SCE-MI implementation, a small single-author reference last touched in 2019 (`narenkn/scemi_lib`), and no open synthesizable transactor library at all, since the AXI, APB and UART BFMs are commercial VIP. Conformance would buy interop with proprietary emulators we can't obtain or test against, which is unfalsifiable and low-value. The spec and its headers are public, so we borrow the shape; the code and the transactor corpus aren't there to reuse. The with-source model to mirror is FireSim/Golden Gate, whose bridges and token channels are the same split in readable code under their own API. Study their channel semantics before inventing ours.

Two costs are real and worth stating to anyone asking. The transactor is ours to write, per protocol, with the untimed side adapted to speak transactions; commercial users escape that only because a marketplace sells them the BFM. And if this is to be more than a demo, the honest investment is an open library of on-ramp-synthesizable protocol transactors, the thing that would produce the examples we currently lack. Anyone hoping to lift a testbench off GitHub and press go should read that as a no.

## Streaming and interactive taps

The output and interactive case is a video renderer, the signal-stream scope (#184), a `--jtag-server` driving OpenOCD in and reading back, or `--trace-signals` observing. These multiply, they run in both directions at once, and they need a structure rather than per-tap socket calls threaded through the loop.

A `Tap` declares what it needs, and the runtime decides how to serve it:

| Field | Meaning |
|---|---|
| `direction` | `Out`, `In` (drives pins), or `InOut` (JTAG) |
| `projection` | the signal set it observes or drives, its slice of state rather than the whole thing |
| `cadence` | every edge, on a strobe, or on a clock domain (JTAG's TCK) |
| `backpressure` | `DropOldest` / `Block` / `Coalesce`, governing the socket-side ring |
| `latency_budget` / `flush_signal` | the tap's intent, which bounds the window; ring depths derive from it |
| `depth` (optional) | override the derived depth for a known workload |

The field that is deliberately absent is a required depth. Per the sizing section, the tap declares intent and the depths follow from the window.

The model produces two FIFO boundaries sized by different constraints. Upstream are the per-tap GPU→CPU rings, generalising the Decision 0013 buffers from UART and bus traces to any projection. Downstream are per-tap socket rings owned by a reactor: one `mio`-style thread over every tap fd, doing accept, read and write, so N taps compose without a thread each and no socket syscall lands on the hot path. The sim loop touches this at two points per batch, draining input rings (blocking only for a gating input) and pushing output rings (never blocking).

The projection is what actually recovers the #184 cost, and it is separate from the reactor. If the GPU packs only a tap's projected signals at the tap's cadence, each ring entry is a few bytes rather than the whole `2×state_size`, so the ring can be deep without the per-edge full readback that was the 50%. Off-thread sockets are cleaner but small; the projection is the win. Keeping them as separate workstreams stops the reactor being credited with a fix it didn't make.

## Consequences

One coherent model covers both cases, because direction, backpressure policy, and endpoint (a GPU-side feeder or the CPU reactor) are parameters of one flow-control design rather than separate designs. A third crossing slots in instead of re-deriving channels, and `BATCH_SIZE` stops being a single wrong number, following the workload and the attached consumers instead.

The costs are equally concrete. Existing UVM drivers don't port; their timed half is rewritten as a BFM (standard SCE-MI and TBX work, but real), and a host adapter over DPI or a socket is still needed on the untimed side. The model suits protocol transactors and targeted taps, not big memories: a BFM carries little state, whereas a 16 MB flash backing store still wants a bespoke kernel rather than an AIG memory, which gives Decision 0013's target architecture a second answer where some peripherals could simply be RTL. It also adds surface to get right, namely a lock-free SPSC ring on the hot path, per-backend GPU-side projection packing, the reactor, and the CPU→GPU feeder. The determinism contract has to be tested rather than assumed, since a live-versus-replay divergence would be silent. When it does fail, the failure is a window pushed past a limit (a starved input, a full output, or memory), reached by over-running the window rather than by picking a wrong buffer size.

## Alternatives considered

A per-cycle bridge (naive cocotb or DPI) is rejected for throughput because it forces `BATCH_SIZE` to 1. It is worth naming honestly, though: this is the signal-level acceleration mode commercial emulators do ship, keeping the driver and co-modelling pin values every clock, with the accelerator idling on the host. Legitimate for bring-up and debug, not a throughput path, and exactly the cliff this ADR avoids.

Full SCE-MI API conformance is deferred rather than rejected. The architecture is shaped to admit it later, but with no open conformant peer and no open transactor corpus there is nothing to validate against, so it would buy interop only with proprietary emulators. Adopt the semantics now and leave the door open.

Running a UVM runtime on the GPU is rejected for the reasons above: it is a SystemVerilog simulator, slower than the thing it replaces. Moving only the socket to a thread is necessary but not sufficient, since it leaves the per-edge readback where the time actually goes. A thread per tap doesn't scale, which is what the reactor is for. Continuous AIMD window control from day one is premature; mode selection plus a flush signal is predictable and likely captures most of the benefit. Record-and-replay works today and stays the recommended interim path, but it isn't reactive, so stimulus can't depend on the design. Vendoring the SCE-MI spec into `docs/` is rejected because it is Accellera's copyrighted work with no update path, and we need the split and the pipes, not the DPI binding details; cite it, don't copy it.

## References

- SCE-MI (Standard Co-Emulation Modeling Interface), Accellera: the [standard downloads](https://www.accellera.org/downloads/standards/sce-mi). v2.4 (2016) is current, and the spec prints the C and SV pipe API in its appendices, including query-only pipe depth (§5.8.5.7) and blocking plus non-blocking transfer with `eom` and auto-flush (§5.8).
- [`narenkn/scemi_lib`](https://github.com/narenkn/scemi_lib): the only open-source SCE-MI implementation found (function-based plus a pipes reference, MIT, single-author, last touched 2019). Useful for API shape and for seeing the structural-endpoint-versus-host-DPI split, not a foundation to build on.
- FireSim / Golden Gate (MIDAS II), UC Berkeley: the same split in open source with code, in [target-to-host bridges](https://docs.fires.im/en/latest/Golden-Gate/Bridges.html) and [target abstraction and host decoupling](https://docs.fires.im/en/latest/Golden-Gate/LI-BDN.html). Read it before designing channel semantics.
- [Testbench Interop](../../interop.md): what Jacquard drives today, and the record-and-replay fallback.
- [Cosim Perf Report](../../cosim-perf-report.md): where the per-edge time goes, and how the GPU-bound claim was measured.
- #184: the signal-streaming and video tap that surfaced the readback-versus-socket distinction.
