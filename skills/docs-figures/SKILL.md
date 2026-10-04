---
name: docs-figures
description: Draw diagrams, charts and CSS animations for GitHub READMEs and documentation as transparent light and dark SVG pairs, with text drawn as glyph outlines so every viewer renders it identically. Use when a README or doc page needs a mechanism or flow diagram, a chart, an animated walkthrough, figures in the ix-docs style, or when a repository's docs/images figures must be regenerated or checked.
compatibility: Python 3.9+ with fontTools. Downloads the style's fonts once (network). Headless Chrome or Chromium for previews.
---

# Docs Figures

If this plugin is not initialized or an Agent IX command fails, read [the dev-tools setup guide](https://github.com/agent-ix/dev-tools/blob/main/setup.md) for its prerequisites and local diagnosis.

The engine lives only in this skill. A repository keeps its figure definitions
(`docs/images/figures.py`) and the generated SVGs, never a copy of the engine.
People without the plugin regenerate from a clone of `agent-ix/dev-tools`.

`<skill>` below is this skill's base directory.

## Workflow

1. **Decide each figure's claim.** One figure shows one mechanism: what moves
   along each edge, which branch is taken, what changes between options. If a
   sentence says it faster, write the sentence. See [design.md](references/design.md).
2. **Write the definitions.** If the repository has no `docs/images/figures.py`,
   copy [assets/starter_figures.py](assets/starter_figures.py) there. Each figure
   is a function returning a `Figure`; list them in `FIGURES`. API:
   [api.md](references/api.md).
3. **Use real values.** Read numbers from the repository's own data
   (recordings, datasets, command output) in the definitions file. Never type a
   value that the repository can supply.
4. **Render:** `python3 <skill>/scripts/render.py docs/images/figures.py`
   (`python3 -m pip install fonttools` first if needed).
5. **Preview, then look:** `python3 <skill>/scripts/preview.py docs/images --out <tmp>`,
   adding `--at 1.5 4 8` for animation frames. Read every PNG. Fix clipping,
   collisions, crowded labels and low contrast before continuing.
6. **Embed** with the `<picture>` pattern in [github.md](references/github.md).
   Give each animated figure a "Show the whole diagram" toggle with its still.
7. **Gate:** `render.py docs/images/figures.py --check` exits 1 when any
   committed SVG is stale. Run it before committing.

## Rules

- A fit check that raises means the layout is wrong: widen the box or shorten
  the text. Never shrink below the style's sizes.
- Colour means role, never decoration. Keep a role's meaning fixed across a
  repository's figures and add a legend when roles repeat.
- Label arrows with what moves along them (`answers`, `0.82`, `true`).
- Each figure has a full-sentence label: the `Figure` label becomes the SVG
  `aria-label`, and Markdown `alt` text copies it word for word.
- Keep output deterministic: no clocks, randomness or unordered iteration.
  Rendering twice must produce identical bytes.
- Animate with CSS only (`lit`/`appear` windows and tokens). No script or SMIL.
  The resting state must be the complete figure, because it is the still and
  the reduced-motion view.
- Headless Chrome does not advance time inside `<img>`. Check animations with
  `preview.py --at`, which seeks inline copies.

## Styles

- `ix-docs` (default): thin coloured boxes with tinted fills, IBM Plex Sans
  Condensed titles and IBM Plex Mono labels.
- `pencil`: hand-sketched. Wobbly double-traced strokes with a subtle tremor,
  open arrowheads, pencil hatching and Kalam handwriting; graphite and coloured
  pencil on light pages, chalk on dark.

A definitions file selects a style with `STYLE = "name"`; `--style` overrides
it. Figures laid out for one style fit the other, so switching is one line.
To add a style, copy a file in `styles/` and change its fonts, sizes, palette
or renderer; keep every role key. See [api.md](references/api.md#styles).

## References

- [api.md](references/api.md): definitions-file contract, `Figure` API, animation, CLI options.
- [design.md](references/design.md): layout grid, roles, charts and animation choreography.
- [github.md](references/github.md): embedding, alt text, stills, GitHub rendering constraints and checks.
