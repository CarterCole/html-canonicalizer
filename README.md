# canon

Recreate a block of HTML with the fewest elements and the least CSS that render the same,
and prove it: the result is compared with the original pixel by pixel.

It is a factorisation. Wrappers that contribute nothing are factored out; what is left are
the elements the picture cannot do without, each carrying only the declarations it needs.

```console
$ canon fixtures/div-soup.html -o card.html
canon: 29 -> 11 elements (62% fewer), 101 declarations, 1.2 kB of markup -> 1.3 kB self-contained
canon: pixel-identical (76860 px compared); 5 layout trials in 1.6s
```

The input is eight nested wrappers around a card, styled by a stylesheet. The output is
the card: no stylesheet, no classes to chase, nothing that is not needed.

```html
<div style="background:#fff;border:1px solid #d1d5db;border-radius:12px;…;padding:20px;width:378px">
  <div style="align-items:center;display:flex;justify-content:space-between">
    <div style="font-size:18px;font-weight:700;line-height:24px;min-width:0">Quarterly report</div>
    <div style="background:#dcfce7;border-radius:999px;…;padding:2px 10px">On track</div>
  </div>
  …
</div>
```

(Indented here for reading; the tool writes it on one line, because white space between
elements can be content.)

| input | elements | result | time |
| --- | --- | --- | --- |
| `fixtures/div-soup.html` | 29 → 11 | pixel-identical | 1.6 s |
| `fixtures/rich-text.html` (editor span soup) | 85 → 35 | pixel-identical | 2.2 s |
| `fixtures/dashboard.html` (grid, table, form controls, SVG, `::before`) | 93 → 71 | pixel-identical | 1.9 s |
| news.ycombinator.com front page | 807 → 411 | pixel-identical, 1.5 M px | 4.6 s |
| a Wikipedia article | 712 → 348 | 33 of 5.7 M px differ (one 12 px icon); exit status 2 | 5.1 s |

## Install

Needs Rust 1.82+ and a Chrome or Chromium on the machine (it drives a headless instance).

```bash
cargo build --release
```

The binary is `target/release/canon`. Chrome is found automatically; point at a specific
one with `--chrome <path>` or `CANON_CHROME`.

## Use

```bash
canon page.html -o min.html
```

```bash
canon https://example.com --selector main --screenshot before.png -o main.html
```

```bash
echo '<div class="a"><div><p>hi</p></div></div>' | canon - --style inline --fragment
```

The input is a file, a URL, literal markup (anything starting with `<`), or `-` for stdin.
Without `--selector` the element recreated is the page's single top-level element, or
`<body>` if there are several.

Useful options (`canon --help` has all of them):

| option | effect |
| --- | --- |
| `-s, --selector <css>` | the element to recreate |
| `--style auto\|inline\|classes` | a class where a block repeats (default), everything inline, or everything in classes |
| `--fragment` | only `<style>` + the element, not a document |
| `--fluid` | leave the width to the container instead of fixing the captured width |
| `--viewport 1280x800`, `--dpr 1`, `--color-scheme light` | how the page is rendered |
| `--no-reduce` | keep every element, only minimise the CSS |
| `--tolerance <n>` | per-channel difference a pixel may have and still count as equal (default 0) |
| `--screenshot`, `--screenshot-result` | save a picture of the original element, and of the recreated one |
| `--diff`, `--report` | save the differing pixels, a JSON report |
| `--time-limit <s>` | stop reducing after this long and keep what is verified (default 120) |

Exit status: `0` the result is verified identical, `2` differences remain (they are
described on stderr and in the report), `1` error.

## What "the same" means

Three independent checks, all against a real browser:

1. **Geometry.** Every box that paints and every line of text lands where the original
   had it, to 1/32 px.
2. **Computed style.** Every element of the result is read back from the browser and
   compared, property by property, with what it was meant to have.
3. **Pixels.** A screenshot of the result is compared with a screenshot of the original
   element; by default not one channel of one pixel may differ.

The claim is for the same browser, machine, viewport and device pixel ratio. When
something cannot be reproduced (see the limits below) the tool says how many pixels
differ and exits with status 2; it does not round a difference down to "identical".

## How it works

Chrome is the layout engine and the judge. The Rust core never decides by itself that
something is redundant; it proposes, and keeps a change only if the browser cannot tell
the difference.

1. **Capture.** A small agent in the page records, for the element and everything under
   it, the *computed* style of every property (via `computedStyleMap()`, which keeps
   `auto`, percentages and `repeat(3, 1fr)` instead of baked pixels), every box, and every
   line of text. Web fonts that actually loaded are carried over, inlined.
2. **Measure the defaults.** In a clean second tab, a bare element of each kind is
   created and read: what a `<div>`, a `<td>`, an `<input>` look like with no styles,
   which properties inherit, which default to `currentcolor`. None of this is a table in
   the source; it is asked of the browser that will render the result.
3. **Faithful copy.** The tree is written out with exactly the declarations that differ
   from those defaults, laid out, and checked. This is the reference for everything after.
4. **Reduce.** Candidate edits are tried in batches and bisected on failure: delete what
   draws nothing, merge a pure layout shell into its only child or parent (moving padding,
   margins, flex and grid properties across), unwrap neutral boxes, join identical inline
   neighbours, remove what is clipped away or off-page (this one needs a screenshot to
   agree). A batch is one render, about 5 ms per thousand elements.
5. **Minimise the CSS.** Declarations that cannot change the picture are dropped (a
   border colour with no border, `box-sizing` on a box nothing sizes). Inherited
   properties are placed by an exact tree optimisation (Sankoff's small-parsimony
   algorithm) so that the fewest declarations give every element the value wherever that
   value shows. Longhands are folded into shorthands, values shortened, repeated blocks
   shared as classes.
6. **Verify.** The finished document is loaded like any page and put through the three
   checks above. A saving that does not survive is taken back, and only that one.

One detail worth knowing: when an element between two runs of text is removed, the runs
would become one, and one run is shaped as a whole, which can move glyphs by a fraction of
a layout unit and change a few antialiasing pixels. Where the screenshot shows that, the
result keeps the two runs apart with an empty comment (`a<!---->b`): zero elements, same
pixels. With `--tolerance` set a little above zero those seams close.

## Limits

The result is a static picture of the element as it was rendered.

- **Not interactive.** Links become `<span>`, hover and focus states are not carried,
  scripts are gone, animations are frozen where they stood.
- **Tags.** Everything becomes `<div>` or `<span>` except what CSS cannot imitate:
  images, SVG, table parts, form controls, `<br>` and a few others. Semantics and
  accessibility are not preserved.
- **Out of its page.** The element's own margins and position are dropped, its width is
  written down (unless `--fluid`), and whatever was painted behind it becomes one flat
  colour.
- **Generated content.** `::before`, `::after` and `::placeholder` are carried over.
  Styles of `::marker`, `::first-line`, `::first-letter` and `::selection` are not.
- **Not reproduced:** content of `<iframe>`s and the current frame of a `<video>`; closed
  shadow roots; scroll positions inside the element; transforms and filters applied by an
  ancestor. Anything of this kind shows up as differing pixels in the verdict.
- **Pages that keep changing** (video, script-driven motion, content that loads late) are
  detected and reported; no pixel claim is made for them.
- Very large pages are slow: each trial lays the whole element out again. Around 2,000
  elements is 10 to 40 s.

## In the page

The same engine runs inside a page as WebAssembly, handed a live element:

```js
import { canonicalize } from './canon.js';

const r = await canonicalize(document.querySelector('.card'));
r.fragment   // <style>…</style><div …>…</div>, ready to paste
r.elements   // 11 (r.sourceElements: 29)
r.verified   // true: every box and line of text lands where the original had it
```

Build it (needs the `wasm32-unknown-unknown` target and the `wasm-bindgen` CLI at the
exact version `crates/wasm/Cargo.toml` pins):

```bash
./web/build.sh
```

`web/dist/` then holds `canon.js`, `canon_core.js` and `canon_core_bg.wasm`; serve them
together. Where the CLI uses a second Chrome tab to try layouts, the page uses a hidden
same-origin iframe the size of its viewport. Trials are built with DOM calls and the
CSSOM, never markup, so they work under a strict Content-Security-Policy and Trusted
Types too.

Options (all optional): `style` (`'auto'`, `'inline'`, `'classes'`), `reduce`, `simplify`,
`fluid`, `timeLimit` (ms, default 3000), `embedFonts`, `classPrefix`, `wasm` (where the
`.wasm` file is, if not next to the module), `settle` (wait for images, fonts and DOM churn
first). The result also has `css`, `html`, `document`, `tree`, `declarations`, `stats`,
`verification` and `warnings`.

How it differs from the CLI:

- **No screenshots.** The result is checked by layout and by reading every computed style
  back, not pixel by pixel. Edits that only a screenshot can justify are not made: removing
  things that are clipped away or off-page, and joining runs of text (which can move
  antialiasing by a pixel). So results can keep a few more elements and an empty comment
  between text runs (`a<!---->b`).
- **It runs on the page's main thread.** The reduction is synchronous and capped by
  `timeLimit`; the page does not respond in the meantime. A 2,000-element subtree takes
  about 1 to 3 s.
- **The page's rules apply.** A Content-Security-Policy without `'wasm-unsafe-eval'` stops
  it with a clear error (Hacker News is one such page). A policy that blocks the script
  itself stops it before it starts (GitHub). Use the CLI there.
- **It leaves the page as it found it.** The frame is removed (also when something fails),
  ancestor transforms that are neutralised for the capture are put back through the CSSOM,
  CSS animations are paused with a temporary stylesheet and released, and lazy-loading
  attributes are restored. Standards, limited-quirks and quirks pages each get a frame in
  their own mode; page CSS zoom is cancelled in the frame.
- **Fonts it cannot carry are reported.** A web font added from script, or defined in a
  stylesheet from another origin, has no readable `@font-face` rule; the result then names
  the family without loading it, says so in `warnings`, and is not `verified`.

Measured by screenshot outside the page (`web/test/inpage.test.mjs`, `web/test/live.mjs`):

| page | elements | time in-page | result vs original |
| --- | --- | --- | --- |
| the three fixtures | as the CLI | under 1 s each | pixel-identical |
| a card on a host with a strict CSP and Trusted Types | fewer | 0.5 s | pixel-identical |
| news.ycombinator.com (CSP bypassed for the test) | 807 → 411 | 0.6 s | pixel-identical |
| a Wikipedia article | 712 → 351 | 1.0 s | 32 of 1 M px (the same icon as the CLI) |

## Layout

```
crates/core   the engine: snapshot model, reduction, CSS minimisation, emitter, fonts.
              Pure: no I/O, no browser. It is handed a `Renderer` to ask questions of.
crates/cli    `canon`: DevTools-protocol client, Chrome launcher, pixel comparison.
crates/wasm   the engine for the browser (wasm-bindgen).
agent/        the page agent: capture on the source, defaults and layout on the replica.
              One file, used by the CLI (injected into tabs) and the web build (inlined).
web/          the in-page build: src/canon.js, build.sh, tests.
fixtures/     inputs used by the end-to-end tests.
```

## Tests

```bash
cargo test
```

```bash
./web/build.sh && node --test web/test/*.test.mjs
```

The first runs unit tests for the CSS folding, geometry, fonts and pixel code, plus
end-to-end tests that run the built binary against the fixtures and require a
pixel-identical verdict. The second runs the in-page build inside the fixture pages and
compares its results with the originals by screenshot, then against hosts built to break
it (strict CSP, Trusted Types with and without a policy, transformed and zoomed pages,
limited quirks, fonts added from script, markup the parser would rebuild) and checks that
each comes back correct or honestly unverified, with the page left as it was. Both need
Chrome and skip themselves without it. `node web/test/live.mjs <url> [selector]` does the
same on any live page.

Set `CANON_TRACE=1` with `-v` to list every edit tried, and `CANON_DUMP=<dir>` to save the
pictures the final comparison was made on.
