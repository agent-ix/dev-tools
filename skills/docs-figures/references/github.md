# GitHub

## Embed a figure

Paths are relative to the Markdown file. GitHub serves the dark source when
the viewer's GitHub theme is dark.

```html
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/NAME-dark.svg">
  <img alt="FULL LABEL" src="docs/images/NAME-light.svg">
</picture>
```

For an animated figure, add the still behind a toggle right after it, with a
blank line around the inner `<picture>`:

```html
<details>
<summary>Show the whole diagram</summary>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/NAME-still-dark.svg">
  <img alt="FULL STILL LABEL" src="docs/images/NAME-still-light.svg">
</picture>

</details>
```

Copy `alt` from the SVG's `aria-label` (the `Figure` label, or `still_label`
for stills). Keep the wording identical; when a label changes, update the
Markdown.

## Why the engine works this way

- GitHub shows repository SVGs through `<img>`, which cannot load web fonts,
  and stylesheets inside the SVG may not fetch fonts either. Glyph outlines
  render the same in every browser and on every GitHub page.
- `<img>` runs no script and takes no clicks or hover, so an image cannot have
  a play or pause button. The still toggle is the substitute.
- CSS keyframes inside the SVG play in `<img>`.
  `@media (prefers-reduced-motion: reduce)` stops them and leaves the resting
  state.
- Backgrounds stay transparent. The palette has separate light and dark
  values because one ink colour cannot read well on both `#ffffff` and
  `#0d1117`.

## Checks

- `render.py FILE --check` before committing.
- If the repository checks Markdown links, make sure the check also scans
  `src` and `srcset` attributes; Markdown-only link regexes miss `<picture>`.
- To see GitHub's sanitised HTML for a page:
  `jq -n --rawfile t README.md '{text:$t, mode:"markdown"}' | gh api markdown --input -`.
  Use `mode: "markdown"`. `gfm` mode turns every newline into a line break.
- Keep each SVG under about 100 KB. Glyphs are defined once and reused; very
  long label text is the usual cause of large files.

## Pitfalls

- Keep values out of hand-typed strings: read them from data so a figure
  cannot disagree with the repository.
- Do not nest same-type quotes inside f-strings in definitions files; that
  needs Python 3.12. Use a local variable.
- A "Show the whole diagram" still must show every branch. If a branch exists
  only in a later pass, draw its edge in the static state too.
- Animated figures show their first-pass values in the still. Mention later
  passes in the animated label, not the still label.
