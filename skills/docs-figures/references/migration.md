# Migrating a repository off the Python figure engine

The Python engine (`figures.py` read by `render.py`, with the `docs_figures` module) is removed from
this skill. A repository that still carries `docs/images/figures.py` moves to a declarative
`docs/images/figures.json` built by `ixd`.

1. Translate each `Figure` in `figures.py` into one entry of `docs/images/figures.json`. Each `box`,
   `line`, `text`, `note`, `dot`, `rect` and `legend` call becomes an item with the same coordinates and
   role. Keep the figure id, width, height and label, so the SVG paths and alt text do not change.
   Shared repository families (TL stack, TL pipeline, TL scenario, Filament ecosystem) are one
   `construct` entry each, for example `{"id": "ecosystem", "construct": {"name": "ecosystem", "here": "REPO"}}`.
2. Bind values the repository can supply (counts, names, verdicts) to its own JSON or source files with
   `data` sources instead of typing them.
3. Add Make targets: `figures` runs
   `ixd build docs/images/figures.json --out docs/images` and `figures-check` runs the same with `--check`.
4. Run `make figures`, review the regenerated SVGs in light and dark, and commit them with
   `figures.manifest.json`. The committed SVG paths stay the same, so README `<picture>` references
   keep working.
5. Delete `docs/images/figures.py` and any committed `__pycache__` for it. Do not copy the engine or its
   styles into the repository.

The old engine's last version is the commit before its removal in agent-ix/dev-tools; use it only to read
an old definition, never to generate new figures.
