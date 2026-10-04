# API

## Definitions file

A repository's `docs/images/figures.py` (any path works):

- Imports from `docs_figures`: `Figure`, size constants (`TITLE`, `LABEL`,
  `SMALL`), `FONTS` and `wrap`. `render.py` puts the engine on the import path
  and activates the style first, so imported constants hold the style's values.
- May set `STYLE = "name"` on its own line. `--style` overrides it. Default:
  `ix-docs`.
- Defines `FIGURES`: a list of functions that take no arguments and return a
  `Figure`. `render.py` writes each figure's outputs next to the file.
- Locates repository data from its own path, for example
  `Path(__file__).resolve().parents[2] / "examples"`.
- Is not runnable on its own; run it through `render.py`.

## Figure

`Figure(name, width, height, label, cycle=None, still_label=None)`

- `name`: output stem; files are `NAME-light.svg` and `NAME-dark.svg`.
- `width`, `height`: drawing area in pixels. The style's margin is added
  around it so strokes never clip. 980 wide fits a README at about 0.9 scale.
- `label`: one or two full sentences stating the figure's claim, with its real
  values. Becomes the `aria-label` and `<title>`.
- `cycle`: animation length in seconds. Required before adding any animation.
- `still_label`: label for the still when the animated label describes motion.

Coordinates are pixels from the top-left corner. Text `y` is the baseline.

### Roles

Colours are named by role, resolved per theme from the style. In `ix-docs`:
`ink`, `muted`, `model`, `logic`, `code`, `evidence`, `faint`. Boxes in `ink`
have no fill; other roles get a tinted fill.

### Primitives

- `text(x, y, s, role="ink", font="mono", size=LABEL, anchor="start", anim=None)`:
  returns the width. `font` is `mono` or `title`. `anchor` is `start`,
  `middle` or `end`. Raises when text leaves the canvas or a glyph is missing.
- `rect(x, y, w, h, role, dashed=False, fill=True, stroke=True, weight=None, opacity=None, rx=4, anim=None)`
- `line(points, role="ink", weight=1.5, dashed=False, arrow=True, anim=None)`:
  an orthogonal polyline from a list of `(x, y)`. End 3 px short of a box edge
  so the arrowhead touches it. Use `weight=2.4` for the main path.
- `dot(cx, cy, r, role, filled=True)`: chart marker.
- `bar(x, y, w, h, role)`: horizontal bar with a rounded data end. Draw a
  `rect(..., "faint", stroke=False, opacity=0.35, rx=2)` track under it.
- `token(points, role, start, end)`: animated dot travelling along `points`
  between `start` and `end` seconds. Hidden in the still.

### Composites

- `box(x, y, w, h, role, title, *subs, dashed=False, sub_role="muted", lit=None)`:
  title in the role colour with muted sub-lines, centred vertically. Raises
  when a line does not fit (16 px padding) or the box is too short. Heights:
  50 to 56 for one sub-line, 62 to 68 for two, 78 for three.
- `note(x, y, s, role, sub=None, anchor="start", anim=None)`: an edge label with
  an optional muted second line 16 px lower.
- `legend(x, y, entries)`: entries are `(role, text, kind)` where `kind` is
  `box`, `solid` or `dash`.

### Helpers

- `wrap(text, width, size=SMALL)`: split text into lines that fit in mono.
- `FONTS["mono"].width(text, size)`: measured width, for layout.

## Animation

Animation is CSS keyframes over one loop of `cycle` seconds. Windows are lists
of `(start, end)` seconds.

- `lit=[...]` on `box`, or `anim=("lit", windows)` on any primitive: dimmed to
  22% outside the windows, full inside, with a 0.2 s fade.
- A title or sub-line given as a list of `(text, windows, static)` variants:
  each appears only during its windows. Use one variant per pass when values
  change between passes. Mark exactly one variant per line `static=True`; it
  is the one shown in the still.
- `anim=("appear", windows, static)` on `text` or `note`: the same, for loose text.
- `token(...)`: one per edge traversal. Light the edge from the token's start
  to the end of the pass.

Without animation every element shows its static state. The still
(`NAME-still-THEME.svg`) is written automatically for animated figures and is
what readers who prefer reduced motion see.

## render.py

`python3 <skill>/scripts/render.py FILE [--out DIR] [--style NAME] [--only NAME ...] [--check]`

- Writes every figure's SVGs to `--out` (default: the file's directory).
- `--only` renders the named figures only.
- `--check` writes nothing and exits 1 when an SVG on disk differs, listing
  each stale file on stderr.

## preview.py

`python3 <skill>/scripts/preview.py PATH ... --out DIR [--width 880] [--at SECONDS ...]`

- `NAME.png`: the light SVG on `#ffffff` above the dark SVG on `#0d1117`, as `<img>`.
- `NAME-THEME@SECONDS.png`: animation frames from inline copies seeked to each time.
- Set `CHROME` when Chrome or Chromium is not on the usual paths.

## Fonts

Fonts download once into `$DOCS_FIGURES_CACHE` (default `~/.cache/docs-figures`).
Text is converted to outline paths, and each glyph is defined once and reused,
so SVGs stay small and need no font at view time.
