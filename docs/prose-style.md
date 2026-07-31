# Prose style

House style for Jacquard's prose docs: the `docs/` book, decision records, and
plans. It covers how sentences read. How docs are *structured* — the reference /
decision / spike split, the three kinds of claim, stable-ID cross-links — lives in
the [decisions README](architecture/decisions/README.md) and the
[redesign spike](architecture/decisions/spikes/architecture-doc-redesign.md); this
page is only about the writing.

## Voice

Be direct and have opinions. State the point first, then support it. Use specific
names and numbers, not vague claims (`BATCH_SIZE = 1024`, not "a fixed batch size").
Trust the reader to see what matters without labels like "significant" or
"important". Write to be understood by a non-native speaker and by an LLM reading a
single retrieved section, so keep each section self-contained and restate its
subject rather than leaning on "as described above".

## Claims track the code

A present-tense sentence in a reference doc is a verifiable claim about the code —
check it before writing it, and keep it current when the code moves. Rationale is
past-tense and lives in decision records, which don't get rewritten when the code
changes. This is the whole point of the layer split; see the decisions README.

## Sentences

- Use contractions: "it's", "don't", "won't".
- Vary sentence and paragraph length. Don't write uniform blocks.
- **Avoid mid-sentence interjections.** Don't bracket a qualifier inside a sentence
  with paired dashes or parentheses. Promote it to its own sentence or move it to
  the end. Write "Only X-capable partitions run the X-aware kernel. In a typical SoC
  after reset, under 5% are X-capable." — not "partitions — typically under 5% after
  reset — run the kernel".
- A single dash introducing an appositive or definition is fine, and is the house
  pattern for naming a thing: "a hierarchical binary tree — the boomerang". Keep it
  to the end of a clause, not the middle of one.

## Lists

Bold-term definition lists are house style for a genuine enumeration where the
reader should weigh each item:

> - **NetlistDB** parses structural Verilog into a flattened database.
> - **AIG** rewrites combinational logic to one uniform node type.

Use them when the terms are the subject. Don't use them as a crutch to avoid writing
a paragraph, and don't pad a two-item list into bullets.

## Banned words and tics

These are the most flagged AI-writing markers. Never use them:

delve, dive into, navigate (figurative), underscore, bolster, foster, harness
(figurative), leverage, unpack (figurative), shed light on, pave the way, pivotal,
groundbreaking, cutting-edge, transformative, game-changing, seamless, intricate,
multifaceted, holistic, testament, landscape (figurative), realm.

And these structures, which mimic insight without providing it:

- "It's not just X — it's Y" / "Not only X, but Y"
- "This isn't about X. It's about Y."
- "It's important / worth noting that…", "At its core…", "When it comes to…"
- Opening with a sweeping statement about the field, or closing with an
  inspirational wrap-up. Start and end on substance.

## Before committing a doc

1. Read it aloud. Does a sentence sound like a press release or a marketing page?
   Rewrite it.
2. Are you saying the same thing twice in different words? Say it once.
3. Is every present-tense claim true of the code as it stands today?
