# Embedding and docs pages

`ixd build docs/images/figures.json --out docs/images` writes `ID-light.svg` and `ID-dark.svg` for each
figure (transparent, text as outlines); a YAML scene build writes `figures/NAME.light.svg` and
`figures/NAME.dark.svg` under its `--out`. Keep these paths stable once a README links them; during a migration
leave the old SVGs in place until the new paths resolve, then update the references.

## README

Paths are relative to the Markdown file. GitHub serves the dark source when the viewer's theme is dark.

A `figures.json` build (primary) writes the pair into `--out` with no subdirectory:

```html
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/ID-dark.svg">
  <img alt="FULL LABEL" src="docs/images/ID-light.svg">
</picture>
```

Copy `alt` from the figure's `label`, word for word; when the label changes, update the Markdown. For an
animated figure (one with `cycle`) embed the still pair, `docs/images/ID-still-light.svg` and
`docs/images/ID-still-dark.svg`, in the README.

A YAML or Rust scene build (secondary) writes `docs/images/NAME/figures/NAME.light.svg` and
`NAME.dark.svg`; use those paths in the same pattern, copying `alt` from the definition's `title`. For
motion link `docs/images/NAME/figures/NAME.animated.svg` (built with `--animated-svg`) and keep the static
pair as the visible image. Link the interactive page (`docs/images/NAME/NAME.html`) as a file for readers
who clone; GitHub does not run scripts in repository files.

## Filament docs (Astro/Starlight)

Embed a published explanation with the MDX wrapper:

```mdx
<Explainer name="NAME" />
```

The wrapper resolves only a named, published explanation and fails the site build with a diagnostic for
an unknown name. It emits the generated static markup, one `<ix-explainer>` with a hashed relative scene
URL, and one shared player script and stylesheet per page, and maps the site theme to the explanation
theme. It adds no `iframe` and no framework runtime. Publish the built directory's `scene/`, `assets/` and
`figures/` into the site's public assets and put the caption and source links in the page text so search
finds them. Status: the wrapper is specified (FR-009, FR-023); confirm it exists in the docs site before
using it, and otherwise embed the `<picture>` pair above.

## Checks

- `make figures-check` before committing.
- If the repository checks Markdown links, make sure the check also scans `src` and `srcset`
  attributes; Markdown-only link regexes miss `<picture>`.
- To see GitHub's sanitised HTML for a page:
  `jq -n --rawfile t README.md '{text:$t, mode:"markdown"}' | gh api markdown --input -`.
  Use `mode: "markdown"`; `gfm` mode turns every newline into a line break.
- Keep each SVG under about 100 KB.
- Scan committed output: no `script`, no event-handler attributes, no external `href`/`src`/`url()`
  beyond in-file fragments (animated SVG requirement FR-037).
