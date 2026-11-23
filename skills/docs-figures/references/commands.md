# Commands

Run every command from the consumer repository root. The consumer root is the current directory, or
`DIR` when `--root DIR` is given (`build` and `validate`). Every source path of a definition is relative
to the root and confined to it, symbolic links included. The definition path and `--out` stay relative
to the current directory, so a repository whose data lives in a subdirectory builds from its own root:
`ixd build schema/docs/images/figures.json --out schema/docs/images --root schema`. A missing source
fails naming the root used and, when the file exists relative to the definition or a parent up to the
git root, that directory: run from there or pass `--root`.

## build and check figures.json (primary)

`ixd build docs/images/figures.json --out DIR [--root DIR] [--check]`. A `.json` definition is built as README figures:
it writes `<id>-light.svg` and `<id>-dark.svg` for each figure (plus `<id>-still-light.svg` and
`<id>-still-dark.svg` for an animated one) and `figures.manifest.json` into `DIR` (default: the definition's
directory). It takes no `--player-dist`, `--animated-svg`, `--scenario` or `--heading-level`. `--check` writes nothing:
it prints `up to date: DIR` or fails with `IXD_CLI_STALE: missing or stale in DIR: FILE, ...`, exit 1,
also for an orphaned output the definition no longer produces. `ixd validate` is for YAML scenes, not
figures.json; an invalid definition fails the build with `IXD_CLI_FIGURES` naming the figure and item.
Reference: [figures-json.md](figures-json.md).

The sections below until "Install and version" describe the secondary YAML scene path.

## validate

`ixd validate docs/diagrams/NAME.yaml [--root DIR]` parses the definition and resolves each declared JSON source
and pointer. Success prints `valid: NAME (N elements, M data source(s))`. Failure prints
`IXD_CLI_INVALID_DEFINITION: FILE:LINE:COL (element.path): error[code]: message`. Codes include
`malformed_yaml`, `unsupported_construct`, `duplicate_id`, `invalid_reference`, `missing_data`,
`invalid_path`, `invalid_json`, `invalid_timeline`.

## build

`ixd build DEF --out DIR [--root DIR] [--player-dist DIR] [--animated-svg] [--scenario ID] [--heading-level N] [--check]`

Writes into `DIR` (default `dist/ixd`):

| Path | Contents |
|---|---|
| `figures/NAME.light.svg`, `figures/NAME.dark.svg` | Static, transparent, theme pair; text as outlines |
| `figures/NAME.fragment.html` | Static HTML fragment for embedding |
| `figures/NAME.animated.svg` | With `--animated-svg`: standalone animated SVG, no script |
| `scene/NAME.scene.json` | Versioned scene the player loads |
| `NAME.html` | Standalone interactive page (opens without a network) |
| `assets/player/<hash>/...`, `fonts/...` | Shared player and the bundled font |
| `manifest.json` | Paths and sha256 of every file |

The default player directory is `packages/player/dist/site` relative to the current directory; outside
an ix-diagrams clone pass `--player-dist` (see Install). Missing player assets fail with
`IXD_CLI_MISSING_PLAYER_ASSETS`. Output is byte-identical for identical input.

### Heading level

`--heading-level N` (2 to 6, default 2) sets the level of the HTML figure's section headings (Captions,
Initial values, Sources, Connections). A host page embeds the figure under its own last heading of
level N-1, so the figure's headings nest below it: under an `<h2>` use `--heading-level 3`. It applies
to YAML and `--scene` builds and to `export`; a `figures.json` build refuses it, and a value outside 2
to 6 fails with `--heading-level requires a number from 2 to 6`. The level is part of the output, so
pass the same value to `--check`.

### Build from a scene

`ixd build --scene SCENE.json [--id NAME] [--title TEXT] [--out DIR] [--player-dist DIR] [--animated-svg] [--scenario ID] [--heading-level N] [--check]`
validates an already-produced scene and packages it in place of a definition, for example the
`scene/NAME.scene.json` of an earlier build or one written by a Rust adapter. `--id` names the outputs
(default: the file name without `.scene.json` or `.json`) and `--title` titles the figure (default: the id).

## check (no-write gate)

The same `ixd build` line with `--check` writes nothing. It prints `up to date: DIR (N files)` and exits
0, or `IXD_CLI_CHECK_FAILED: DIR is out of date (stale: FILE, ...); run ixd build` and exits 1.
Pass the same `--animated-svg` and `--scenario` flags you built with. Run the check in the Make gate
and in CI.

## export

`ixd export DEF --format svg|animated-svg|html [--scenario ID] [--heading-level N] [--out DIR] [--player-dist DIR]`
writes only the requested format (default `dist/ixd-export`), for example one animated SVG of the
`late` scenario: `ixd export docs/diagrams/NAME.yaml --format animated-svg --scenario late --out docs/export --player-dist PLAYER`.
Use `export` for a one-off share; use `build` for committed output.

## preview and playback

`ixd preview docs/images/NAME [--port N]` serves the built directory on `127.0.0.1` (default 4173)
after verifying its manifest. Open `http://127.0.0.1:4173/NAME.html`. The player offers play and pause,
step navigation, scenario selection, configuration choices, linked highlighting and expansion.
Check each scenario plays to its end, that reduced-motion shows the complete final state, and that the
static fallback is readable with scripting off.

## Install and version

There is no registry release yet and the repository is private, so installation needs git access to
`agent-ix`.

- `figures.json` builds need only the binary. With a git credential for `agent-ix` (for example `gh auth setup-git`):
  `CARGO_NET_GIT_FETCH_WITH_CLI=true cargo install --locked --git https://github.com/agent-ix/ix-diagrams ix-diagrams --bin ixd`.
- YAML scene and `--scene` builds also need the player, which `cargo install` does not provide. Use a clone:

1. Clone `agent-ix/ix-diagrams` beside the consumer.
2. `cargo build --release` gives `target/release/ixd`; put it on `PATH` or set `IXD=`.
3. `make player-install player-build` gives `packages/player/dist/site`; set `IXD_PLAYER=` to it.
4. `ixd --version` prints the package version (0.1.0 at the time of writing).

For Rust adapters add `ix-diagrams = { git = "https://github.com/agent-ix/ix-diagrams", rev = "<full sha>" }`
to `Cargo.toml`; Cargo's `rev` and lockfile are the only place the version is pinned. Build the
player and the binary from the same revision. A new ix-diagrams revision may change output bytes;
rebuild with `make figures` and review the diff. Never copy engine sources into the consumer.

## Style choice

Roles (`ink`, `muted`, `model`, `logic`, `code`, `evidence`, `faint`) keep one meaning across styles. A
`figures.json` selects the style with a top-level `"style": "ix-docs"` (default, clean vector) or
`"pencil"` (hand-sketched), applied to every figure in the file. YAML scene builds have no style
selector and render the shared default tokens (`assets/styles/tokens.css` in ix-diagrams).
