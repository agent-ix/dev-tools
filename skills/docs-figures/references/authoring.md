# Authoring a figure

README and docs-page figures use `docs/images/figures.json`, documented in [figures-json.md](figures-json.md).
This page covers the two secondary paths for explanatory scenes: a YAML scene and a Rust adapter. They
build the same validated model and scene contract; neither evaluates scripts.

| Path | Use it for | Where it lives |
|---|---|---|
| Declarative YAML scene (secondary) | Generic diagrams: cards, boundaries, annotations, labelled connections, steps and scenarios | `docs/diagrams/NAME.yaml`, built with `ixd build` |
| Rust authoring (secondary) | Rich-card, state, sequence, architecture, swimlane and quantitative families, and any producer adapter | A small Rust program in the consumer (or the `ix-diagrams` facade calls) that writes the scene and SVG |

For an ordinary figure stay on figures.json. For a scene, start with YAML and move to Rust only when the claim needs a family the YAML element kinds cannot
express (see [families.md](families.md)). The worked examples of the Rust path in
`ix-diagrams` are `crates/ixd-example-fsl` (rich cards) and `crates/ixd-example-arch`
(architecture); read them before writing a new adapter.

## Declarative definition

`schema_version: 1`. Top-level keys: `id`, `title`, `semantic_profile` (optional `id`/`version`),
`data_sources`, `components`, `elements`, `steps`, `scenarios`. Unknown keys are rejected.

Element `kind` is one of `card`, `field`, `boundary`, `connection`, `annotation`.

- `card`: a drawn box. `title` is its heading; `fields` are shown values.
- `boundary`: encloses elements whose `parent_id` names it. Set the literal field
  `boundary_kind` to `trust`, `deployment` or `ownership`; keep trust and deployment separate.
- `connection`: an edge. Literal fields `source` and `target` must name drawn elements; `title` is
  the label, so write what moves along it.
- `annotation`: free-standing note.
- `field`: a labelled value inside a card; it is not drawn on its own.

A `step` names the `element_ids` that matter and a `caption`. A `scenario` orders `step_ids`, may carry
`configuration` choices, and may add `transitions` (`duration_ms`, `easing`) and timeline `actions`
that change a field over time. Every scenario shown together must use one input and configuration;
never combine results from different inputs under one outcome.

Start from [../assets/starter-definition.yaml](../assets/starter-definition.yaml).

## Real data binding

Declare each producer output under `data_sources` with a repository-relative `path` (no `..`). A
field takes either a literal or a binding:

```yaml
fields:
  verdict: {type: data, source_id: run, pointer: /items/0/category}   # RFC 6901 JSON pointer
  label:   {type: literal, value: clause}
```

- Run the real producer, store its JSON, bind pointers into it. Do not retype results.
- `ixd validate` reads the data and fails with `missing_data` if a pointer does not resolve, so a
  producer change that moves a field is caught at once.
- `citations: [{source_id: run}]` records provenance on the element. A source of `kind: trace` also
  requires a stable `record_id` on each citation; use `kind: data` for a plain result document.
- Re-run the producer and rebuild when its input changes; `--check` reports the figure stale.
- If the producer exits non-zero for a legitimate verdict (for example `fsl run` exits 10 for a
  refuted clause), keep the output and let the Make recipe ignore that status with a leading `-`.

## Rust authoring

The facade crate re-exports the pieces: `ix_diagrams::parse_definition_yaml`, `DefinitionBuilder`,
`ElementDefinition`, `ElementKind`, and the module crates `constructs`, `layout`, `scene`,
`render`, `timeline`. Build the same definition in Rust with:

```rust
use ix_diagrams::{DefinitionBuilder, ElementDefinition, ElementKind};

let definition = DefinitionBuilder::new("service", "Request path")
    .with_element(ElementDefinition::new("client", ElementKind::Card, "Client"))
    .build()?;
```

Rich families come from `ix_diagrams::constructs`:

- Cards: `CardDeclaration { id, kind: CardKind, fields }`, `build_card(&decl, &CardLayout)`,
  `render_card_svg`, `validate_scene_cards`. Kinds: state, evaluation, queue, worker, domain-object,
  evidence, code.
- Architecture: `ArchDeclaration` list, `validate_arch`, `build_arch`, `select_path`.
- Process / swimlanes: `ProcessDeclaration` with lanes, steps, decisions, gates, handoffs.
- Traces (state replay, sequence): `TraceDeclaration`, `TraceBuilder`.
- Quantitative marks: `MarkDeclaration`, `build_mark`, `render_mark_svg`.

Rules for a Rust adapter: map producer fields only (no semantics of your own), take every value from
the producer output, attach `Provenance` to each entity, measure text with the bundled font catalog
(`FontCatalog::with_bundled_math()`), and expose `generate` and `check` commands so Make can call them.
The adapter may be a `[[bin]]` or an integration test in the consumer. Pin `ix-diagrams` in
`Cargo.toml` by git `rev`; that pin is the only place the version is recorded.
