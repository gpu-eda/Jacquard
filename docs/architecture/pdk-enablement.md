# PDK enablement

*Reference — the current design as it stands in the code. The why lives in the
decision records linked from each section; the not-yet-built work is under
[Implementation status](#implementation-status). Present tense means "true
today"; if a claim here is stale, the code moved and this doc didn't.*

A PDK gives Jacquard two things about every cell type it contains: the
combinational or sequential **logic** the [simulation engine](simulation-engine.md)
splices into the design's AIG, and the **timing** characterization a `--timing-report`
run reads. Both come from one source, a per-library **cell-model IR descriptor**
(`liberty-to-cellir` converter, [Decision 0019](decisions/0019-cell-model-ir.md)),
and both the built-in PDKs and a user's own foundry library go through the same
descriptor mechanism. Cells a descriptor doesn't cover, such as third-party IP,
memory macros, and filler and pad cells, get the same kind of declarative
treatment through a second, smaller path: a hand-authored TOML manifest
([Decision 0010](decisions/0010-declarative-cell-metadata.md)). One mechanism for
generated, per-library data; one for hand-declared, per-cell data. Neither
requires a Jacquard code change to onboard a new cell.

```d2
vars: { d2-config: { pad: 16 } }
direction: down
netlist: "Netlist cell type"
resolve: "Descriptor resolution\n(explicit > bundled > prefix > fallback)"
descriptor: "Cell-model IR descriptor\n(L1 pins, L2 AIG, L3 seq roles, L4 timing)"
manifest: "Runtime manifest\n(--cell-library + .cells.toml)"
aig: "Design AIG"
netlist -> resolve -> descriptor -> aig: splice pre-built AIG
netlist -> manifest -> aig: opaque output / explicit RAMBlock
```

## The cell-model IR descriptor

Every combinational and sequential cell type is looked up in a **cell-model IR
descriptor**: a generated, versioned JSON file carrying everything Jacquard needs
about a library, keyed by cell type — pin directions (L1), the cell's logic as a
**pre-decomposed AIG** (L2), sequential pin roles and classification (L3), and
per-corner timing characterization (L4). Construction of the design AIG
(`AIG::from_netlistdb`) looks up each cell type in the resolved descriptor and
splices its pre-built AIG straight in; there's no per-cell decomposition work at
simulation start.

The `liberty-to-cellir` converter (`crates/liberty-to-cellir`) produces a
descriptor from a Liberty `.lib`, consulting a functional `.v` only as a fallback
or cross-check for logic Liberty under-specifies. It derives the corner from the
Liberty's operating conditions, compiles each cell's `function` string to an AIG,
and extracts `ff`/`latch` sequential roles. A library with multiple corners keeps
all of them in the one descriptor, keyed by corner name; a `--corner <name>` run
flag picks which one to simulate against, defaulting to the descriptor's declared
`default_corner`. The simulation corner is always user-selected — never inferred
from the netlist (synthesis's setup corner is recorded nowhere Jacquard reads) or
from an SDF header, since an SDF's delays already annotate a specific design's
instances rather than describe the library.

A netlist's descriptor is resolved in order: an explicit `--cell-descriptor
<file.json>`, an explicit `--bundled-descriptor <name>`, auto-match by the
descriptor's declared cell-name prefix, then a default-fallback descriptor for
cells with no vendor prefix. This is how AIGPDK is selected: its cells share no
common prefix, so it's the fallback rather than a prefix match. A netlist mixing
cell-name tracks that both match a prefix (for example GF180MCU's 7-track and
9-track libraries in one design) is ambiguous and auto-selection rejects it —
pick the library explicitly instead.

## Declarative cell metadata for cells outside a descriptor

Third-party IP, hard macros, and other cells that don't need new AIG decomposition
rules take a second, lighter path: a user-supplied Verilog file
(`--cell-library <path>.v`, repeatable) gives `sverilogparse` the cell's pin
directions, and a co-located `<library>.cells.toml` manifest (autoloaded, or
passed via `--cell-manifest`) declares each cell's `kind` — `ram`, `filler`,
`io_pad_input`, `clock_gate`, and so on. This is the `RuntimeCellLibrary` path
(`src/cell_library.rs`, [Decision 0010](decisions/0010-declarative-cell-metadata.md)):
no Jacquard PR to add a cell, just a manifest shipped alongside the design. Where
a manifest and a resolved descriptor could both classify a cell, the manifest is
the hand-authored override layer over the generated IR, not a competing source.

## RAM and SRAM macros

A manifest entry with `kind = "ram"` has two modes. Without a `[cells.NAME.ram]`
sub-table it's **opaque**: the cell's outputs become X-source slots and no memory
behavior is modeled, which is enough for a design that never reads back what it
wrote to that memory at the timescales Jacquard simulates. With the sub-table
present it's **explicit**
([Decision 0011](decisions/0011-ram-port-mapping-schema.md)): pin roles for clock,
chip-enable, write-enable, write-mask, address, and data are declared, and the
cell gets a real AIG-backed `RAMBlock` with per-entry backing storage — writes
populate it, reads return what was written. `sim_config.json`'s `sram_init` can
then preload that storage from an ELF's `PT_LOAD` segments before tick 0.

## The built-in PDKs

Four cell libraries ship as bundled descriptors: **AIGPDK** (Jacquard's own
synthesis library of AND gates, DFFs, clock gates, and SRAMs, and the
default-fallback descriptor), **SKY130**, **GF180MCU**, and **IHP SG13G2** — the
last onboarded purely by vendoring its Liberty and generating a descriptor, with
no per-PDK Rust written for it. Each is regenerated in CI from its pinned
vendored submodule and embedded into the binary at build time rather than
committed as a generated blob, so a released `jacquard` binary carries no runtime
dependency on the vendored PDK sources.

## Adding a new or custom PDK

The full step-by-step procedure is in [`docs/adding-a-pdk.md`](../adding-a-pdk.md):
vendoring a library's Liberty as a submodule, running the converter, registering a
bundled descriptor, or pointing `--cell-descriptor` at a generated file for a
library that can never be vendored. That guide also documents a
legacy hand-written-Rust workflow (per-PDK pin tables, classifiers, and
decomposition functions); it's kept as a reference for the machinery the
descriptor replaces, not a path to take for a new PDK.

## Private and NDA-bound PDKs

A contributor with commercial-PDK access under NDA can't commit that PDK's
Liberty, SDF, or other characterization files to the public repository. The
private-PDK test track ([Decision 0004](decisions/0004-private-pdk-testing.md))
gates such tests on a per-PDK environment variable pointing at a licensed
directory: tests skip cleanly with a "PDK not available" message when the
variable is unset, and run fully when it points at a readable PDK. Only the test
harness and PDK-agnostic structural fixtures are committed — never a PDK-derived
artifact.

## Constraints

- A cell type must ultimately resolve to a `DriverType` the design AIG
  understands (`AndGate`, `DFF`, `SRAM`, or `Tie0`), inherited from the
  [simulation engine](simulation-engine.md)'s synchronous, edge-triggered model.
  A cell matching neither a descriptor, the legacy per-PDK fallback, nor a
  manifest `kind` fails cell recognition.
- The RAM port-mapping schema is opinionated: single-port (1RW), synchronous
  only, and a write-mask that's bit- or byte-granular, not arbitrary. Depth is
  capped at `2^AIGPDK_SRAM_ADDR_WIDTH` (8192 entries today) and width at 32 bits.
  Multi-port and asynchronous SRAMs are out of scope.
- `sram_init` ELF preload works only for a single-SRAM design today; matching
  ELF segments to multiple SRAM instances by address is not yet built.
- A commercial library whose functional model is one flat-module `.v` file
  skips the converter's logic cross-check at generation time — the descriptor
  still gets emitted, but that cross-check needs validating through simulation
  instead.
- The private-PDK test track's open-source path (`GF180MCU_LIBERTY_DIR`) is
  wired; the NDA-gated, per-vendor `*_PDK_PATH` pattern for commercial PDKs is
  not.

## Implementation status

Built and in use: descriptor-driven combinational logic for all four built-in
PDKs and for any library reachable via `--cell-descriptor`; CI-regenerated,
build-time-embedded bundled descriptors with prefix auto-selection and explicit
override; the `--cell-library` / `.cells.toml` runtime manifest path with the
`kind` discriminator; the explicit RAM port-mapping schema with real backing
storage and single-SRAM ELF preload; and the open-source-PDK slice of the
private-PDK test track.

Decided but not yet built:

- **Descriptor-driven sequential logic for every built-in PDK.** GF180MCU
  consumes its descriptor's sequential roles today; the other built-ins still
  route some flip-flop and clock-gate variants through preserved hand-written
  pin-name matches. [Decision 0019](decisions/0019-cell-model-ir.md).
- **The NDA-gated commercial-PDK testing track** — per-vendor `*_PDK_PATH`
  environment variables, licensed-runner gating — described in the decision but
  not implemented; only the open-source GF180MCU path ships.
  [Decision 0004](decisions/0004-private-pdk-testing.md).
- **Multi-SRAM `sram_init` targeting** — matching ELF segments to more than one
  SRAM instance by virtual-address overlap. [Decision 0011](decisions/0011-ram-port-mapping-schema.md).
- **Flat single-module functional-`.v` cross-checking** at descriptor-generation
  time, for commercial libraries that ship one undifferentiated file rather than
  one file per cell. [Decision 0019](decisions/0019-cell-model-ir.md).
- **A descriptor-backed pin-direction provider**, retiring the remaining
  build-time pin-table generation for cells the descriptor's L1 already covers.
  [Decision 0019](decisions/0019-cell-model-ir.md).

## Decisions behind this

- [Decision 0004](decisions/0004-private-pdk-testing.md) — private PDK testing
  track: env-gated per-PDK test suites so NDA-bound PDK files never enter the
  public repository.
- [Decision 0010](decisions/0010-declarative-cell-metadata.md) — declarative
  cell metadata: the `--cell-library` + `.cells.toml` manifest path, classifying
  third-party cells by `kind` without a Jacquard code change.
- [Decision 0011](decisions/0011-ram-port-mapping-schema.md) — RAM port-mapping
  schema: promotes a manifest-declared RAM from opaque to explicit-port, with
  real backing storage.
- [Decision 0019](decisions/0019-cell-model-ir.md) — cell-model IR: one
  generated, per-library descriptor carrying pin directions, combinational logic
  as a pre-built AIG, sequential roles, and per-corner timing, consumed at
  runtime with no per-PDK Rust.
