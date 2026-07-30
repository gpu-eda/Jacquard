# Spike — Architecture documentation redesign

**Status:** exploring. This is a throwaway test of a doc structure, not a
migration. Nothing is renamed until the shape proves out on a real area.

## The problem

Three separate complaints point at the same fix.

"ADR" is jargon. The phrase "Architecture Decision Record" doesn't tell a
newcomer what's inside, and enough people bounce off the term that it's worth
dropping.

An ADR conflates three kinds of claim that [age at different
rates](../README.md#three-kinds-of-claim-aged-differently): why a decision
was made, what the code does now, and what's decided but unbuilt. Bundling them
in one document is what makes ADRs go stale. The *why* is historical and should
never be rewritten; the *what* has to track the code; the *unbuilt* part shrinks
as things land. One document can't age all three well.

There's no by-area entry point. The ADRs are a numbered pile. A reader who wants
"how does cosim I/O work today" has to know it's spread across 0013, 0017 and
0022 and reconstruct it.

## The shape

Split the pile along the seam that already exists in the three-buckets model:
what the code is now, versus why it got that way.

- **Architecture reference — the *what*.** One document per area, describing the
  current design as it stands in the code. This is the top-level entry point,
  organised by area, and the thing a reader lands on first. It's living: kept in
  step with the code, rewritten in place when the code moves.
- **Decision log — the *why*.** The rationale and the alternatives rejected,
  each entry tied to the moment it was decided. This is what the ADRs already
  are; they become the decision log, reframed. Historical by nature, written in
  past tense, not rewritten when the code changes.
- **Status — the *unbuilt*.** The gap between a decision and the code. Lives in
  an `Implementation status` section on the architecture doc (and a `plans/`
  entry for the work), and drains into the *what* as each piece lands.

**Spikes stay, as the evidence.** A spike is neither a *what* nor a *why*; it's
the experiment a decision rested on. They keep their own home and get referenced
*from* the decisions they informed, the way [ADR
0003](../archive/0003-opentimer-primary-sta.md) already cites the
[OpenTimer spike](opentimer-sky130.md) that killed it. A decision cites its
evidence; the spike itself is a dated record and doesn't get rewritten.

The link runs both ways. Each architecture section points to the decisions that
shaped it, so a reader on the current shape can reach the reasoning in one hop.
Each decision points forward to the architecture doc it produced, so a reader in
the log can see what became of it, and back to the spikes it rested on. The
*whats* point to the *whys*, and the *whys* point to their evidence.

This is deliberately less destructive than renaming every ADR into an area
document. The decision records keep their numbers and their content; they move
under a "decision log" heading and get forward-links. The new writing is the
architecture layer on top.

[Diátaxis](https://diataxis.fr/reference-explanation/) gives this split its
name and its rule. Its *reference* type (what you consult while working:
factual, present-tense, structured for lookup) is the architecture layer; its
*explanation* type (background, the why, written for study) is the decision log.
Diátaxis names our exact failure mode too: explanation bleeds into reference,
dragging time-bound claims into a doc that's supposed to state what is. The rule
that follows is the whole anti-staleness law: no rationale, and no dead history,
in a reference doc.

## What this revises in the current convention

The [ADR README](../README.md) currently says ADRs are *living documents,
edited in place to stay current*. That rule exists only because the ADR is the
sole doc, so it has to carry current state, so it has to be edited when the code
moves. Add a reference layer that carries current state, and the pressure to
edit the decision record vanishes.

So the redesign flips the mutability rules by layer, and the flip is the point:

- The **architecture reference** is living and present-tense, edited in place.
  The "keep it current" rule moves here, where it belongs.
- The **decision log** goes back to immutable and past-tense: a decision is a
  record of a choice at a moment, not a description of the system now. A changed
  decision is a new record that supersedes the old, not a rewrite. This is the
  Nygard model, and it's what stops a decision record from rotting: history
  can't go stale.

This does partly reverse the living-ADR convention I committed earlier, and
that's worth saying plainly rather than quietly. It doesn't bring back the thing
we actually disliked, dated "Amendment" blocks accreting inside one document,
which both models reject. It changes only what happens when a *decision* itself
changes: edit the reference doc to match the code, and write a new decision
record for the changed why.

## Naming and titles

Files use `<class><number>-<name>.md`:

- `arch-3-cosim-runtime.md` — an architecture-reference doc.
- `dec-0022-flow-controlled-io.md` — a decision-log entry (was Decision 0022).

The number is a stable ID, not a reading order. Decision entries keep their
existing four-digit ADR numbers, so every "Decision 0022" reference already in the
docs, commits and issues still resolves. Architecture docs get their own short
sequence per the taxonomy below.

The filename is for identity and stable links. The *title* a reader sees is set
by the `SUMMARY.md` link text and the document's `# H1`, both ours to choose, so
the rendered nav can read "Cosim runtime" while the file stays
`arch-3-cosim-runtime.md`.

## The doc tree

"Architecture" is the top-level home, chosen over "ADR" precisely because the
term is opaque to newcomers. Everything hangs under it: the map first, then the
current-state docs, then the decision log, with spikes nested under the log as
the evidence decisions rested on.

```
docs/architecture/
  README.md                    # the map — by-area index, the first thing a reader hits
  arch-1-timing-correctness.md # current state, the "what", one per area
  arch-3-cosim-runtime.md
  ...
  decisions/                   # the decision log — the "why" (today's ADRs, reframed)
    README.md                  # decision index (today's adr/README.md)
    dec-0013-...md
    dec-0022-...md
    spikes/                    # the evidence a decision rested on
      spike-amd-laptop-backend.md
      spike-architecture-doc-redesign.md
    archive/                   # reversed/dropped decisions (today's adr/archive/)
```

Rendered nav (`SUMMARY.md`) mirrors it, with human titles:

```
# Architecture
- [Overview](architecture/README.md)      # the map
  - [Timing correctness](architecture/arch-1-timing-correctness.md)
  - [Cosim runtime](architecture/arch-3-cosim-runtime.md)
  - ...
- [Decisions](architecture/decisions/README.md)
  - [0013 — Cosim peripheral model architecture](...)
  - ...
  - Spikes
    - [Reaching AMD laptops](...)
    - ...
```

Two placement calls in this tree are worth stating rather than defaulting.
*Spikes live under `decisions/`* because a spike is the experiment a decision
rested on, so it belongs beside the decision that cites it, not floating at the
top level as today. *The decision log is a subdirectory of `architecture/`*, not
a sibling, so "Architecture" is genuinely the single top-level home and the why
sits one level below the what it explains. Both are reversible; neither blocks
the taxonomy work.

## Taxonomy

The areas are the load-bearing decision. Six candidates, mapping every current
ADR to a home:

| # | Area | Current-state doc | Decisions behind it |
|---|---|---|---|
| 1 | Timing correctness | `arch-1-timing-correctness.md` | 0001, 0002, 0005, 0006, 0008, 0009, 0019; roadmap 0007 |
| 2 | Simulation engine | `arch-2-simulation-engine.md` | 0014, 0015, 0016 |
| 3 | Cosim runtime | `arch-3-cosim-runtime.md` | 0012, 0013, 0017, 0022 |
| 4 | RTL on-ramp | `arch-4-rtl-onramp.md` | 0021 |
| 5 | PDK enablement | `arch-5-pdk-enablement.md` | 0004, 0010, 0011 |
| 6 | Distribution | `arch-6-distribution.md` | 0018, 0020 |

Two placements are genuinely ambiguous and worth deciding rather than guessing.
Cell metadata (0010, 0011) is written by PDK enablement but read by timing, so
it could sit in either. The X-propagation work (0016) spans the engine and
cosim. Overlaps like these are the real cost of an area split, and where a
framework's guidance (below) should earn its keep.

## Worked example: cosim runtime

`arch-3-cosim-runtime.md` would open on the current design and carry the
cross-links:

```markdown
# Cosim runtime

Cosim runs reactive peripheral models as GPU kernels alongside the design, so
inputs can depend on design outputs cycle by cycle. [What the code does today,
in the present tense: the batch dispatch loop, the multi-clock scheduler, the
peripheral model trait, the GPU→CPU ring buffers.]

The batch is the core constraint: `BATCH_SIZE` scheduler edges run with the CPU
absent. See [dec-0017](...) for why the loop is shaped this way, and
[dec-0013](...) for the peripheral model architecture the rings come from.

## External I/O

[Current state once built. Today this links to dec-0022 as decided-but-unbuilt.]

## Implementation status

- Batch dispatch, multi-clock scheduler, peripheral rings — built
  (dec-0013, dec-0017).
- Flow-controlled I/O across the batch boundary — decided, unbuilt
  (dec-0022, plan: ...).
```

The decision entries lose nothing. `dec-0022` keeps the full rationale it has
now and gains one forward-link: "current state: arch-3, External I/O." This area
is the sharp test because it exercises all three buckets at once — built
peripherals, a proposed I/O design, and the why behind both.

## Diagrams

No modelling tool. Structurizr, C4-as-code and the like earn their keep when one
model feeds dozens of views that must stay mutually consistent as it changes,
which is a system-of-services problem. Jacquard is one binary with a compute
pipeline; the model fits in a doc, so a modelling tool buys single-source views
we don't need at the cost of a DSL and a build step. Same reasoning as rejecting
arc42's full apparatus.

The firm rule is diagrams as text, never checked-in images. A text diagram
diffs in a PR, lives next to the prose, and is readable by an agent; a PNG rots
invisibly, hides what changed from a reviewer, and is an opaque blob to a model.
Keep them sparse and follow C4's discipline: one abstraction level per diagram,
and the diagram shows *what*, never *why*. A straight pipeline stays as an ASCII
block (zero-dependency, reads fine); reach for a rendered diagram only where the
shape is genuinely two-dimensional (the backend/peripheral/ring relationships,
the three-tier model).

The format choice is **D2**, decided by rendering the same real diagram (the
cosim batch boundary) both ways and comparing. Both are declarative text, so the
AI reader is indifferent; the choice turned on layout quality and the render
path.

- **D2** kept a nested node inside its container box, coloured the three
  containers cleanly, and routed the boundary-crossing edges with few crossings.
  It needs the `d2` binary wherever docs build (`mdbook-d2` shells out to it),
  and GitHub won't render it inline in a PR.
- **Mermaid** floated an unconnected node far from its group, its subgraph
  colouring didn't take, and the layout was more cramped on the same graph.
  It renders client-side with no binary and inline on GitHub.

The one cost of D2, no inline GitHub render, is minor here: the canonical
rendered form is the mdBook site on GitHub Pages, not GitHub's Markdown preview,
and both render there. The layout gap is real and lands exactly on the 2-D shapes
that are the only diagrams we'll draw. `mdbook-d2` is wired into `book.toml` (with
`layout = "elk"`, which beat dagre on the container shapes) and the docs CI job;
`theme/d2-diagrams.css` strips the code-block chrome the preprocessor wraps each
SVG in. The worked cosim doc carries the first diagram as a live test of the path.

Two techniques from getting the first diagram to look right, both reusable:

*Theme-matching.* D2 bakes one palette into the SVG, so a light diagram lands on
a navy page (mdBook's default dark theme). `theme/d2-diagrams.css` repaints every
shape from the active theme's CSS variables (`--fg`, `--bg`) — author CSS
overrides D2's baked `fill=`/`stroke=` — so the diagram follows a live theme
switch. Three gotchas worth knowing before the next diagram: D2 sets shape colour
via an embedded `.d2-<hash> .stroke-B1` rule whose specificity (0,2,0) beats
element selectors, so shape colours need `!important`; the connection `<mask>`
contains unclassed `<rect>`s, so a bare `rect` fill rule corrupts the mask and
clips every line — scope to `rect[class]`; and per-region accent colour needs a
hook D2 doesn't otherwise give, so the three regions carry a *sentinel* fill in
the source that CSS remaps to a translucent (hence theme-adaptive) tint.

*Verifying style without a live browser.* The diagram is an SVG styled by page
CSS, so a static D2 render can't show the real result — render the built page in
headless Chrome instead (`--headless --screenshot`, or `--dump-dom` after an
injected `getComputedStyle` probe to read the actual applied stroke/fill). Seed
`localStorage['mdbook-theme']` before the page's boot script to force a theme.
This caught the invisible-lines bug that a D2 PNG render hid completely.

Legibility has one non-obvious lever: on-screen text size is the native font
scaled by `column_width / diagram_native_width`, because the SVG is fit to the
content column. A wide diagram shrinks its text into the floor. Bumping the D2
font doesn't help (boxes grow with it, so the ratio is unchanged) — the fix is to
keep the diagram *narrow*, laying regions out vertically and reserving horizontal
flow for genuine pipelines. The cosim diagram at ~1177px native keeps text near
two-thirds of body size in the column; a wide left-to-right version of the same
graph dropped it to a quarter. Prefer tall over wide: vertical scroll is natural
on the web, horizontal isn't.

## What this tests

- Does the *what* doc read as a coherent whole, or just a stitched-together
  summary of its decision records?
- Is the taxonomy stable, or do the ambiguous placements (cell metadata,
  X-prop) mean the areas are wrong?
- Does the two-way linking stay maintainable, or does it rot the moment a
  decision moves?
- Is the migration worth it against just renaming the ADR section "Decisions"
  and adding a by-area index?

## What the field does, and what to take from it

A survey of the usual frameworks (sources at the end) mostly confirms the
split and lends a few concrete shapes. The one-line reading of each:

- **ADRs (Nygard).** The five-field core (title, status, context, decision,
  consequences) is all a decision record needs; MADR's option-by-option
  apparatus is worth it only for a genuinely contested call. The staleness
  everyone complains about is a category error: an ADR is point-in-time history,
  and history doesn't rot if you write it in past tense and supersede rather than
  edit. Link each record to the PR that implemented it. Take the core, the
  immutability, and the PR link.
- **Diátaxis.** The reference-versus-explanation cut is the theoretical spine of
  this whole redesign, as above. Take it as the organising rule.
- **arc42.** Don't adopt the 12 sections. Five earn their keep as the skeleton
  for each architecture-reference doc: a building-block view (the pipeline, one
  level deep), crosscutting concepts (the things that span modules: multi-backend
  kernel parity, AIG invariants, the partition resource limits), constraints
  (synchronous logic only), risks and tech debt, and a glossary. The glossary
  matters more than usual here because the domain is jargon-dense (AIG, boomerang
  stage, repcut, endpoint group) and an LLM reader hallucinates undefined terms.
  Skip runtime, deployment, and formal quality-scenario sections.
- **C4.** Not as a framework. Two habits only: one abstraction level per diagram
  (a single Level-1 pipeline diagram, then per-stage zoom-ins, never one
  god-diagram), and diagrams show *what*, never *why*.
- **RFCs / design docs (Rust model).** This is the home for decided-but-unbuilt:
  an accepted-but-unimplemented proposal plus a tracking issue is exactly bucket
  three, and the tracking link is what makes it self-liquidating as work lands.
  We already have `docs/plans/` reaching for this; formalise the link, skip the
  process ceremony (no voting, no FCP).
- **ISO 42010.** Ignore the standard. Steal one idea: every view exists to answer
  a named reader's concern, and one of our readers is now an LLM coding agent.

The AI-reader findings are the part with the least settled evidence and the most
direct payoff, so they're worth stating as rules rather than aspirations:

- **Self-contained sections.** An agent retrieves a chunk, not the tree. Each
  section should stand alone: restate its subject, avoid "as described above."
  This is the same discipline as Diátaxis reference writing.
- **Stable-ID cross-links, never prose pointers.** "See dec-0021" resolves; "see
  the synthesis decision" doesn't. This is why the decision numbers stay stable.
- **No stale present-tense claims in a reference doc.** A human skims past a wrong
  claim; a model repeats it as generated code. That raises the cost of the
  staleness failure and is the strongest reason to keep the reference layer
  ruthlessly current and rationale-free.
- **Curate, don't generate.** One measured study
  ([arXiv 2601.20404](https://arxiv.org/html/2601.20404v2)) found LLM-written
  context files *reduced* task success in most settings. Write only what an agent
  can't get by reading the code.
- **A one-screen map.** A `docs/architecture/README.md` an agent hits first,
  linking every area doc and its decisions by stable ID.

`llms.txt` is the one piece to skip unless we publish a docs site; it solves
HTML-ingestion, which an in-repo Markdown reader doesn't have. The cross-tool
`AGENTS.md` convention (shared agent context, tool-specifics left in `CLAUDE.md`)
is worth adopting on its own track, separate from this.

On naming, the field backs the scheme already chosen, with one refinement:
encode the class as a *directory*, not a filename prefix. `docs/architecture/`,
`docs/architecture/decisions/` and `docs/plans/` carry the class; the file inside is just
`NNNN-kebab-title.md`. Cite by number, keep the number out of the H1 title, and
assign the number at merge to dodge cross-PR collisions.

## Sources

- Diátaxis, [reference vs explanation](https://diataxis.fr/reference-explanation/).
- [Nygard ADRs](https://martinfowler.com/bliki/ArchitectureDecisionRecord.html) and [MADR](https://adr.github.io/madr/).
- [arc42](https://arc42.org/overview); [C4 model](https://c4model.com).
- [RFCs vs design docs vs ADRs](https://newsletter.pragmaticengineer.com/p/rfcs-and-design-docs); [Rust RFCs](https://github.com/rust-lang/rfcs).
- AI-reader: [AGENTS.md](https://agents.md/), [llms.txt](https://llmstxt.org/), and the efficacy study [arXiv 2601.20404](https://arxiv.org/html/2601.20404v2) (treat its exact percentages as indicative, one 2026 paper).
