---
name: docs-figures
description: Draw diagrams, charts and CSS animations for GitHub READMEs and documentation as transparent light and dark SVG pairs, with text drawn as glyph outlines so every viewer renders it identically. Use when a README or doc page needs a mechanism or flow diagram, a chart, an animated walkthrough, figures in the ix-docs style, or when a repository's docs/images figures must be regenerated or checked.
compatibility: The `ixd` binary from agent-ix/ix-diagrams (Rust toolchain to build it) and a built player directory. No Python.
---

# Docs Figures

If this plugin is not initialized or an Agent IX command fails, read [the dev-tools setup guide](https://github.com/agent-ix/dev-tools/blob/main/setup.md) for its prerequisites and local diagnosis.

Figures are drawn with [ix-diagrams](https://github.com/agent-ix/ix-diagrams) (`ixd`). Public reference:
the [README](https://github.com/agent-ix/ix-diagrams/blob/main/README.md) (definition format and Rust
authoring), [BUILDING.md](https://github.com/agent-ix/ix-diagrams/blob/main/BUILDING.md) (build and player),
and the [examples catalog](https://github.com/agent-ix/ix-diagrams/blob/main/examples/README.md). A
repository keeps its figure definitions and the generated output, never a copy of the engine. Output is
deterministic: the same definition and data give byte-identical files, so `--check` can gate commits.

There is one primary path and two secondary ones; say which one a figure uses:

- **`docs/images/figures.json` (primary, for READMEs and docs pages):** a declarative file of figures
  drawn with boxes, lines, text, columns, code panels and animation, with values bound to the repository's
  own files, and shared constructs (`ecosystem`, `tl-stack`, `tl-pipeline`, `tl-scenario`). Built by
  `ixd build docs/images/figures.json --out docs/images` into `<id>-light.svg` and `<id>-dark.svg`. Full
  reference: [figures-json.md](references/figures-json.md). Use it for every ordinary figure.
- **YAML scene (secondary, explanatory scenes):** `docs/diagrams/NAME.yaml` with `card`, `boundary`,
  `connection` and `annotation` elements, steps and scenarios, built with `ixd build` into an interactive
  player page. Use it when readers should step through scenarios.
- **Rust adapter (secondary, rich scenes):** a small Rust program (a `[[bin]]` with `generate` and `check`
  commands, as in ix-diagrams `ixd-example-fsl`, `ixd-example-arch`, `ixd-devteam-example`) that maps
  real producer output to rich cards, state replay, architecture, swimlanes or marks.

Both secondary paths are recorded in [recipes.md](references/recipes.md).

A repository that still has `docs/images/figures.py` follows [migration.md](references/migration.md); the
old Python engine is removed from this skill.

## Workflow

1. **Decide each figure's claim.** One figure shows one mechanism: what moves along each edge, which
   branch is taken, what changes between options. If a sentence says it faster, write the sentence.
2. **Bind real values.** Point `data` sources at the repository's own JSON, source or corpus files; add
   an `expect` where the figure claims something a source decides. Never type a value the repository can
   supply. See [figures-json.md](references/figures-json.md#sources-data).
3. **Write `docs/images/figures.json`:** copy a figure from a sibling repository or start from
   [assets/starter-figures.json](assets/starter-figures.json). Give each figure a full-sentence `label`.
4. **Build:** `ixd build docs/images/figures.json --out docs/images`. Fix every diagnostic; each names
   the figure and the item.
5. **Look at it** in light and dark: clipping, crowded labels, low contrast.
6. **Embed** with the `<picture>` pattern in [github.md](references/github.md). The `alt` copies the
   figure's `label` word for word.
7. **Gate:** add the targets from [assets/figures.mk](assets/figures.mk) to the repository Makefile;
   `make figures-check` runs `ixd build ... --check`, which writes nothing and exits 1 on any stale,
   missing or orphaned file. Run it before committing.

For an explanatory scene with steps and scenarios (YAML) or a rich family (Rust), follow
[authoring.md](references/authoring.md) and [recipes.md](references/recipes.md).

The secondary paths have worked examples, including a create-then-revise walkthrough, in
[recipes.md](references/recipes.md).

## Rules

- Colour means role (`ink`, `muted`, `model`, `logic`, `code`, `evidence`, `faint`), never decoration.
  Keep a role's meaning fixed across a repository's figures.
- Label arrows with what moves along them (`answers`, `0.82`, `true`).
- Each figure has a full-sentence title; it becomes the accessible name, and Markdown `alt` copies it.
- Keep output deterministic: no clocks, randomness or unordered iteration in Rust authoring programs.
- Never hand-edit generated output. Change the definition or the data and rebuild.
- Do not add Python or logic-bearing shell. Rust programs hold logic; Make targets only call `ixd`.
- The resting state is the complete figure, because it is the still and the reduced-motion view.

## Install and version

`ixd` has no registry release yet and `agent-ix/ix-diagrams` is private. `figures.json` builds need only the
binary: `CARGO_NET_GIT_FETCH_WITH_CLI=true cargo install --locked --git https://github.com/agent-ix/ix-diagrams ix-diagrams --bin ixd`
(with a git credential for `agent-ix`). YAML scene builds also need the player, so build from a clone of
`agent-ix/ix-diagrams` (`cargo build --release` gives `target/release/ixd`; `make player-install player-build`
gives the player at `packages/player/dist/site`). Check with `ixd --version` and record the ix-diagrams revision in the
consumer's Cargo pin or CI setup, not in the figures. Details and the
Rust dependency form: [commands.md](references/commands.md#install-and-version).

## Styles

`figures.json` takes a top-level `"style": "ix-docs"` (default, clean vector) or `"pencil"` (hand-sketched);
roles keep one meaning in both. The YAML and Rust paths render the shared default tokens and have no style
selector.

## References

- [figures-json.md](references/figures-json.md): the primary path: command, outputs, items, sources, expressions, repeat, animation, constructs.
- [authoring.md](references/authoring.md): the secondary YAML scene and Rust adapter paths.
- [families.md](references/families.md): which diagram family fits which claim.
- [commands.md](references/commands.md): build and check of figures.json, validate, export, preview, playback, install, style.
- [recipes.md](references/recipes.md): story recipes and the recorded create/revise walkthrough.
- [github.md](references/github.md): README embedding, `<Explainer>` docs wrapper, link checks.
- [design.md](references/design.md): layout and labelling guidance.
