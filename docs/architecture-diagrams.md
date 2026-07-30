# Authoring architecture diagrams

Diagrams in the docs are written as [D2](https://d2lang.com) text and rendered to
inline SVG at build time. They are theme-adaptive: colours and fonts come from the
active mdBook theme, so a diagram reads correctly in light, navy, coal, ayu, and
rust, and recolours on a live theme switch. This page is the how-to; the *why*
(D2 over mermaid, the layout choices) is in
[the redesign spike](architecture/decisions/spikes/architecture-doc-redesign.md).

## Adding a diagram

Write a fenced `d2` block in any `docs/*.md` file:

````markdown
```d2
vars: { d2-config: { pad: 16 } }
direction: down

a: First box
b: Second box
a -> b: does a thing
```
````

The [`mdbook-d2`](https://github.com/danielweck/mdbook-d2) preprocessor (configured
in `book.toml`) shells out to the `d2` binary and replaces the block with an inline
SVG. Nothing else is needed — no image files, no per-diagram wiring.

## Authoring rules

These keep diagrams legible and consistent. They come out of the spike; follow them
unless you have a specific reason not to.

- **Lay regions out vertically; reserve horizontal flow for genuine pipelines.**
  On-screen text size is the native font scaled by
  `content_column_width / diagram_native_width` — a wide diagram shrinks its text
  into an unreadable hairline when fit to the column. A tall, narrow diagram keeps
  text near body size and scrolls vertically, which is natural on the web. Set
  `direction: down` at the top level and only use `direction: right` inside a
  container that is a real left-to-right pipeline.
- **One abstraction level per diagram.** A single overview, then per-stage detail
  if needed — never one diagram that tries to show everything.
- **Show *what*, not *why*.** The rationale belongs in the decision record the
  diagram's page links to, not in the picture.
- **Keep `pad` small.** `vars: { d2-config: { pad: 16 } }` trims D2's default 100px
  canvas border (mdbook-d2 exposes no pad config, so set it in the source).
- **Do not set colours or fonts in the D2 source.** The stylesheet owns them (next
  section). The one exception is a *sentinel fill* used as a colour hook.

The global layout engine is `elk` (set in `book.toml`), which handles containers
and edge routing better than the default dagre.

## How theming works

D2 bakes one fixed palette into the SVG. `theme/d2-diagrams.css` repaints it from
the active mdBook theme's CSS variables (`--fg`, `--bg`) — author CSS overrides the
SVG's baked `fill=`/`stroke=` attributes, so no SVG post-processing is needed. The
same stylesheet strips the `<pre>` code-block chrome that mdbook-d2 wraps each SVG
in, and scales the diagram to the content column.

Three details in that stylesheet are load-bearing, and each fixes a real bug — know
them before you touch it:

- **Shape colours need `!important`.** D2 sets shape colour through an *embedded*
  `<style>` using `.d2-<hash> .stroke-B1`-style selectors, specificity (0,2,0),
  with a per-diagram hash we can't match directly. Element selectors score lower
  and lose, so shape fill/stroke overrides use `!important`. (Text scores high
  enough via `.text` to win without it.)
- **Match `rect[class]`, never bare `rect`.** D2's connection arrows use an SVG
  `<mask>` whose unclassed `<rect>`s mean "visible here / hole there". A bare `rect`
  rule repaints those mask rects too, turning the mask opaque and clipping every
  connection line to nothing — invisible arrows whose stroke still *computes*
  correctly. Rendered shapes always carry a D2 colour class; mask rects never do,
  so `rect[class]` skips them.
- **Region accents use a sentinel fill.** D2 gives no stable per-shape hook (user
  classes aren't emitted; the auto-generated classes are base64 of the shape path).
  To colour a specific region, set a sentinel hex in the D2 source
  (`style.fill: "#4f8bff"`) and remap it in CSS
  (`rect[fill="#4f8bff" i] { fill: rgba(...) !important; ... }`). The remap is a
  *translucent* tint, which is what makes it read over both light and dark
  backgrounds. The cosim diagram uses `#4f8bff` / `#3cbe78` / `#aa78f0` for its
  three regions.

## Verifying a diagram's styling

A D2 PNG render (`d2 file.d2 out.png`) shows the *baked* palette, not the themed
result, so it cannot confirm the styling. Render the built page in headless Chrome
instead:

```sh
mdbook build
chrome=/Applications/Google\ Chrome.app/Contents/MacOS/Google\ Chrome
"$chrome" --headless=new --disable-gpu --force-device-scale-factor=2 \
  --window-size=1400,2600 --screenshot=/tmp/page.png \
  "file://$PWD/book/architecture/<page>.html"
```

To force a theme, seed `localStorage` before mdBook's boot script runs by injecting
`<script>localStorage.setItem('mdbook-theme','navy')</script>` right after `<head>`
in a copy of the built HTML. To read the *computed* style of an element (e.g. to
confirm a connection line's stroke actually resolved), inject a `getComputedStyle`
probe that writes the result into `document.title` and read it back with
`--dump-dom`. This is how the invisible-arrows bug was found — the PNG render hid it
completely.

## Build dependencies

The docs build needs the `d2` binary and the `mdbook-d2` preprocessor. Locally:

```sh
brew install d2            # or: curl -fsSL https://d2lang.com/install.sh | sh
cargo install mdbook-d2
```

CI installs both in the `Documentation` job (see `.github/workflows/ci.yml`): `d2`
from its release tarball into `~/.cargo/bin`, and `mdbook-d2` from crates.io.
