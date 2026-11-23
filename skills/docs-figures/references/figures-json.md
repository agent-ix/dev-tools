# figures.json: the README and docs figure path

`docs/images/figures.json` is the primary way a repository draws README and documentation figures. One
file lists every figure of the repository; `ixd build` writes a light and a dark SVG for each. The
definition is data: it holds no script, and values come from the repository's own files. The
authoritative source is the `ixd-figures` crate in agent-ix/ix-diagrams (`definition.rs` for the
fields, `bind.rs` for sources, `expr.rs` and `expand.rs` for expressions and loops).

## Build and check

```bash
ixd build docs/images/figures.json --out docs/images            # write
ixd build docs/images/figures.json --out docs/images --check    # write nothing, exit 1 if stale
ixd build schema/docs/images/figures.json --out schema/docs/images --root schema --check   # sources under schema/
```

Run from the repository root. The consumer root is the current directory, or `DIR` with `--root DIR`; source
paths are relative to the root and may not contain `..` or resolve outside it, symbolic links included. The
definition path and `--out` stay relative to the current directory. A missing source fails naming the root
used and, when the file exists relative to the definition or a parent up to the git root, that directory
(run from there or pass `--root`). A `.json` definition is built as figures. `--out` defaults to the
definition's directory. Nothing is written when any figure fails. Output is byte-identical for
identical input.

| Output | Meaning |
|---|---|
| `<id>-light.svg`, `<id>-dark.svg` | Transparent theme pair for each figure; text is glyph outlines |
| `<id>-still-light.svg`, `<id>-still-dark.svg` | Only for an animated figure (one with `cycle`): the resting frame for READMEs |
| `figures.manifest.json` | Files of the last run; the next run deletes listed files the definition no longer produces and `--check` reports them as orphaned. Other files are never touched |

`--check` prints `up to date: DIR` or `IXD_CLI_STALE: missing or stale in DIR: FILE, ...` and fails.
Commit the SVGs and the manifest. Wire the two targets in [assets/figures.mk](../assets/figures.mk).

## File shape

```json
{
  "style": "ix-docs",
  "data": { "NAME": { "from": "SOURCE", "...": "..." } },
  "figures": [ { "id": "hero", "width": 980, "height": 240, "label": "Full sentence.", "items": [] } ]
}
```

Unknown keys are rejected everywhere.

- `style`: `ix-docs` (default, thin coloured boxes with tinted fills) or `pencil` (hand-sketched
  double-traced strokes and hatching). It applies to every figure in the file.
- `data`: named values read from the repository (below).
- `figures`: drawn in order. Each has `id` (the output stem), `width`, `height`, `label` (the accessible
  name; the README `alt` copies it word for word), `items`, and optionally `let`, `cycle`,
  `still_label`, `construct`. `width` and `height` may be `=` expressions. `width`, `height` and
  `label` are required unless a `construct` supplies them.

Roles are `ink`, `muted`, `model`, `logic`, `code`, `evidence`, `faint`. Colour means role, never
decoration.

## Items

Every item is an object with a kind `k`. Coordinates are canvas pixels, origin top left.

| `k` | Fields (required first) | Notes |
|---|---|---|
| `box` | `at [x,y,w,h]`, `role`, `title`; `subs`, `dashed`, `sub_role`, `lit` | Tinted node. A box with sub-lines needs enough height (about 56 for one) or the build fails with "box is too short" |
| `note` | `at [x,y]`, `text`, `role`; `sub`, `anchor`, `anim` | Label on an edge or area |
| `text` | `at [x,y]`, `text`; `role`, `font` (`mono`/`title`), `size`, `anchor` (`start`/`middle`/`end`), `when`, `anim` | Free text |
| `rect` | `at [x,y,w,h]`, `role`; `dashed`, `fill`, `stroke`, `weight`, `opacity`, `rx`, `anim` | Grouping frames, cells |
| `line` | `points [[x,y],...]`, `role`; `weight`, `dashed`, `arrow` (default true), `anim` | Polyline; the arrowhead is at the last point |
| `dot` | `at [cx,cy,r]`, `role`; `filled`, `when` | Circle |
| `bar` | `at [x,y,w,h]`, `role` | Horizontal bar with a rounded data end |
| `legend` | `at [x,y]`, `entries [[role,label,kind],...]` | `kind` is `box`, `dash` or `fill` |
| `columns` | `at [x,y,w,h]`, `values`, `role`; `labels`, `ticks`, `value_labels`, `gap` | Vertical column chart. `values` are numbers or names of bound numeric values; `role` is one role or one per column; `ticks` mark values with faint guides on the left |
| `code` | `at [x,y,w]`, `lines`; `title`, `highlight`, `callouts`, `line_height`, `pad`, `role`, `wrap` | Source panel. A line is plain text or `[[text, role], ...]` spans. `highlight` is `{keywords, keyword, string, other}` roles; `callouts` are `{line, n, role, note}` rings in the right gutter. A line wider than the panel fails unless `wrap` is true |
| `para` | `at [x,y]`, `text`, `width`; `role`, `line_height`, `size` | Text wrapped to a width |
| `token` | `points`, `role`, `start`, `end` | Dot travelling the path between two seconds of the cycle (animation) |
| `repeat` | `over`, `items`; `as`, `index`, `let`, `when` | Unrolls items over a list (below) |

Any item may carry `when` (a bound value name, `true`/`false`, or an `=` expression); it is drawn only
when true. Every text field (`title`, `subs`, `text`, `labels`, `note`, ...) may interpolate values.

## Sources (`data`)

Each entry is `{"from": KIND, ...}`. Paths are repository-root-relative; globs likewise.

| `from` | Fields | Gives |
|---|---|---|
| `json` | `path`, `pointer`; `join`, `length`, `raw` | Value at a JSON Pointer. `join` joins an array of scalars; `length` gives the element or key count; `raw` keeps lists and records for expressions instead of text |
| `regex` | `path`, `pattern`; `group`, `all`, `names`, `join`, `dedupe`, `allow_empty` | Capture group of the first match; with `all`, a list of every match (records keyed by `names` when given) |
| `count` | `glob` | Number of files matching the glob |
| `matches` | `glob`, `pattern` | Total regex matches across the matching files |
| `files` | `glob` | Sorted list of matching file paths |
| `dirs` | `path` | Sorted names of the immediate subdirectories |
| `text` | `value` | A literal |
| `sum` | `of [names]` | Sum of numeric values |
| `sha256` | `of`; `head`, `tail` | Digest of another value, optionally shortened to `head...tail` |
| `expr` | `value` | Result of an expression (number, text, list or record), without the leading `=` |
| `find` | `path`, `pointer`, `key`, `equals`; `field` | Array element whose `key` equals `equals` (by value, never position), or a JSON Pointer `field` inside it |
| `tally` | `path`, `pointer`; `filters [{key, equals, prefix}]` | Count of array elements passing every filter |
| `expect` | `of`; `equals` or `like` | Fails the build unless the named value equals the text (or the value `like` names); the value is that value. Use it to pin a figure's claim to a source |
| `enum-variants` | `path`, `name`; `plain` | Number of variants of `pub enum NAME` in a Rust file |
| `default-field` | `path`, `type`, `field`; `grouped` | Numeric default of a field in `impl Default for TYPE` |
| `read` | `dir`, `of` | Trimmed text of `<dir>/<value of>` |
| `nth` | `path`, `pattern`, `index_of`; `exclude` | The index-th match (group 1) of a regex, skipping matches containing `exclude` |
| `node`, `lookahead`, `instants`, `cell`, `held`, `trace-csv`, `signal` | see `definition.rs` | TL corpus readers for formula nodes, traces and signal catalogs; use them only in TL repositories |

A bound value that does not resolve fails the build, so a producer change that moves a field is caught
at once. Never type a value the repository can supply.

## Expressions

A string starting `=` is an expression (`"=10 + 66 * i"`); `==` starts a literal `=`. In labels,
`{expr}` or `{expr:.N}` interpolates (`N` is 0 to 12 decimals, or `g` for plain); `{{` and `}}` are
literal braces. Expressions are a closed vocabulary: they read bound values and loop variables, never a
file, the network or the clock, and cannot define functions or loops.

- Values: numbers, quoted text, `true`/`false`, lists `[a, b]`, records. `rec(name, value, ...)`
  builds a record. Access `x.field`, `x[i]` (negative counts from the end).
- Operators, loosest to tightest: `or`, `and`, `not`, `== != < <= > >=`, `+ -` (`+` also joins text and
  lists), `* / // % `(`//` floors), unary `-`. `if(cond, a, b)` is the conditional.
- Numbers: `sum min max abs round floor int fmt(x, digits)`.
- Text (also map over lists): `str lower upper trim squash replace(x, from, to) rsub(x, regex, to)
  split(x, sep) starts ends matches(x, regex) bytes index(x, needle) head(x, n) findall(x, regex)`.
- Lists: `len first last range(n) slice(l, from, to) join(l, sep) flatten unique keep(l, regex)
  drop(l, regex) contains(l, v) pluck(l, path) where(l, path, v) find(l, path, v) get(x, path)
  sort_by(l, path, "desc") position(l, regex) pick(rec, keys) count_by(items, keys) zip_join(a, b, sep)`.
- `ensure(cond, message)` fails the build with the message when false.

Limits: an expression is at most 2048 characters and nests at most 64 deep; `range` and `repeat`
handle at most 10,000 elements; a figure expands to at most 10,000 items; `repeat` nests at most 8
deep; a regex compiles to at most 1 MiB; one SVG is at most 8 MiB.

## repeat, let, when

```json
{"k": "repeat", "over": "cases", "as": "c", "index": "i", "let": {"y": "=10 + 66 * i"},
 "items": [{"k": "box", "at": [10, "=y", 200, 56], "role": "logic", "title": "{c.id}", "subs": ["{c.status}"]}]}
```

`over` names a bound list or is an `=` expression; `as` (default `row`) and `index` (default `i`) name the
element and its position; `let` computes per-element values. Figure-level `let` computes values for the
whole figure, each after the ones it reads (a cycle fails). `when` skips the whole block.

## Animation

Set `cycle` (seconds) on the figure to animate it, and `still_label` for the resting frame's accessible
name. Elements show over windows `[[from, to], ...]` in seconds: `lit` on a `box` dims it outside its
windows; `anim` on `note`, `text`, `rect` and `line` takes `lit`, `appear` (hidden outside the windows)
and `still` (an `appear` element the still frame shows); a `box` line may be a list of variants
`{text, windows, still}`; `token` moves a dot along `points` between `start` and `end`. The still SVGs
(`<id>-still-light.svg`, `<id>-still-dark.svg`) are for README embedding; the animated pair is for pages.
The resting state must be the complete figure.

## Shared constructs

A figure may be only `{"id": "ecosystem", "construct": {"name": "ecosystem", "here": "REPO"}}` and
inherits canvas, label and items. `here` highlights the repository the figure sits in.

| `name` | Figure |
|---|---|
| `ecosystem` | The Filament repository map (main chain, orchestration, shared libraries) |
| `tl-stack` | The TL crate dependency graph |
| `tl-pipeline` | The TL formula pipeline with an oracle cross-check |
| `tl-scenario` | The TL debugging walkthrough as a sequence diagram; `bound: true` reads the case from bound values `scn_start`, `scn_end`, `scn_prop`, `scn_verdict`, `scn_time`, `scn_buffer` |

## Authoring rules

- One figure shows one mechanism. Label arrows with what moves along them.
- Bind every value the repository can supply (`json`, `regex`, `count`, `expect`); type only concepts.
- Use `expect` to make a figure fail to build when the source it describes changes.
- Never hand-edit the SVGs; change the definition and rebuild.
- Interactive and scenario-driven explanations are a different tool: see the secondary paths in
  [authoring.md](authoring.md).
