# Legacy Python docs-figures engine (deprecated)

This directory holds the original Python figure engine, its styles, starter, API reference and tests.
It is deprecated. FR-034 (IXD-34) keeps it available until every canonical consumer's default branch has
migrated to ix-diagrams (FR-010-AC-8); the IXD-34 retirement change deletes this directory, preserves
committed SVGs and adds a migration guide.

Do not use it for new figures, and do not copy it into a repository. New work follows
[../SKILL.md](../SKILL.md) with `ixd`.

Layout: `scripts/` (engine), `styles/`, `assets/starter_figures.py`, `references/api.md` (contract),
`tests/` (its own tests).
