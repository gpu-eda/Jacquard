# Architecture

The current design of Jacquard, by area. Each area is a **reference** doc: what
the code does today, present tense, no rationale and no dead history. The *why*
behind each choice lives in the [decision records](decisions/README.md), linked from
the section it explains. The *not-yet-built* work lives in each doc's
Implementation-status section and in [plans](../plans/README.md).

If you're an agent or a new reader, start here: this page maps every area to its
current-state doc and the decisions behind it.

| Area | Reference | Decisions behind it |
|---|---|---|
| Timing correctness | [timing-correctness](timing-correctness.md) | 0001, 0002, 0005, 0006, 0008, 0009, 0019; roadmap 0007 |
| Simulation engine | [simulation-engine](simulation-engine.md) | 0014, 0015, 0016 |
| Cosim runtime | [cosim-runtime](cosim-runtime.md) | 0012, 0013, 0017, 0022 |
| RTL on-ramp | [rtl-onramp](rtl-onramp.md) | 0021 |
| PDK enablement | [pdk-enablement](pdk-enablement.md) | 0004, 0010, 0011, 0019 |
| Distribution | [distribution](distribution.md) | 0018, 0020 |

*Decision 0019 (cell-model IR) is cross-cutting: it supplies both a cell's logic
(PDK enablement) and its timing characterization (timing correctness), so both
rows list it. See [the redesign spike](decisions/spikes/architecture-doc-redesign.md)
for the design.*
