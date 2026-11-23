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

There are two authoring paths; say which one a figure uses:

- **YAML path (generic elements):** `docs/diagrams/NAME.yaml` with `card`, `boundary`, `connection` and
  `annotation` elements, built by `ixd build`. Use it for simple boxes-and-arrows claims.
- **Rust path (rich families):** a small Rust adapter (a `[[bin]]` with `generate` and `check` commands, as
  in ix-diagrams `ixd-example-fsl`, `ixd-example-arch`, `ixd-devteam-example`) that maps real producer output
  to rich cards, state replay, architecture, swimlanes or marks. Use it whenever the figure needs those
  families. Both walkthroughs are recorded in [recipes.md](references/recipes.md).

A repository that still has `docs/images/figures.py` follows [migration.md](references/migration.md); the
old Python engine is removed from this skill.

## Workflow

1. **Decide each figure's claim.** One figure shows one mechanism: what moves along each edge, which
   branch is taken, what changes between options. If a sentence says it faster, write the sentence.
   Choose the family with [families.md](references/families.md).
2. **Get the producer's real output.** Run the tool that decides the result (for example
   `fsl run REQUEST --format json > docs/data/run.json`) and keep the JSON in the repository. Never
   type a value the producer can supply. See [authoring.md](references/authoring.md#real-data-binding).
3. **Write the definition.** Copy [assets/starter-definition.yaml](assets/starter-definition.yaml) to
   `docs/diagrams/NAME.yaml` and bind each shown value to a JSON pointer. Rich-card, state, sequence,
   architecture, swimlane and quantitative families are Rust authoring; see
   [authoring.md](references/authoring.md).
4. **Validate:** `ixd validate docs/diagrams/NAME.yaml` (run from the repository root). Fix every
   diagnostic; each names the file, line, column and field path.
5. **Build:** `ixd build docs/diagrams/NAME.yaml --out docs/images/NAME --player-dist PLAYER_DIR`.
   Add `--animated-svg` for a standalone animated SVG. Commands and flags:
   [commands.md](references/commands.md).
6. **Preview, then look:** `ixd preview docs/images/NAME`, open the printed URL, play the scenario,
   and inspect light and dark. Fix clipping, crowded labels and low contrast before continuing.
7. **Embed** with the `<picture>` pattern or the `<Explainer>` wrapper in
   [github.md](references/github.md). Alt text copies the figure's title word for word.
8. **Gate:** add the targets from [assets/figures.mk](assets/figures.mk) to the repository Makefile;
   `make figures-check` runs `ixd build ... --check`, which writes nothing and exits 1 on any stale or
   missing file. Run it before committing.

Follow a full example in [recipes.md](references/recipes.md), including a create-then-revise
walkthrough.

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

`ixd` has no registry release yet; build it from a clone of `agent-ix/ix-diagrams`
(`cargo build --release` gives `target/release/ixd`; `make player-install player-build` gives the player
at `packages/player/dist/site`). Check with `ixd --version` and record the ix-diagrams revision in the
consumer's Cargo pin or CI setup, not in the figures. Details and the
Rust dependency form: [commands.md](references/commands.md#install-and-version).

## Styles

Colour roles are fixed by the visual language (FR-008) and cover the `ix-docs` and `pencil` styles.
The current CLI renders one style and has no style flag; see
[commands.md](references/commands.md#style-choice) for what to do today.

## References

- [authoring.md](references/authoring.md): definition format, data binding, Rust authoring, citations.
- [families.md](references/families.md): which diagram family fits which claim.
- [commands.md](references/commands.md): validate, build, check, export, preview, playback, install, style.
- [recipes.md](references/recipes.md): story recipes and the recorded create/revise walkthrough.
- [github.md](references/github.md): README embedding, `<Explainer>` docs wrapper, link checks.
- [design.md](references/design.md): layout and labelling guidance.
