# Commands

Run every command from the repository root; data and definition paths resolve against the current
directory and may not escape it.

## validate

`ixd validate docs/diagrams/NAME.yaml` parses the definition and resolves each declared JSON source
and pointer. Success prints `valid: NAME (N elements, M data source(s))`. Failure prints
`IXD_CLI_INVALID_DEFINITION: FILE:LINE:COL (element.path): error[code]: message`. Codes include
`malformed_yaml`, `unsupported_construct`, `duplicate_id`, `invalid_reference`, `missing_data`,
`invalid_path`, `invalid_json`, `invalid_timeline`.

## build

`ixd build DEF --out DIR [--player-dist DIR] [--animated-svg] [--scenario ID] [--check]`

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

## check (no-write gate)

The same `ixd build` line with `--check` writes nothing. It prints `up to date: DIR (N files)` and exits
0, or `IXD_CLI_CHECK_FAILED: DIR is out of date (stale: FILE, ...); run ixd build` and exits 1.
Pass the same `--animated-svg` and `--scenario` flags you built with. Run the check in the Make gate
and in CI.

## export

`ixd export DEF --format svg|animated-svg|html [--scenario ID] [--out DIR] [--player-dist DIR]`
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

There is no registry release yet.

1. Clone `agent-ix/ix-diagrams` beside the consumer.
2. `cargo build --release` gives `target/release/ixd`; put it on `PATH` or set `IXD=`.
3. `make player-install player-build` gives `packages/player/dist/site`; set `IXD_PLAYER=` to it.
4. `ixd --version` prints the package version (0.1.0 at the time of writing).

For Rust adapters add `ix-diagrams = { git = "https://github.com/agent-ix/ix-diagrams", rev = "<full sha>" }`
to `Cargo.toml`; Cargo's `rev` and lockfile are the only place the version is pinned. Build the
player and the binary from the same revision. A new ix-diagrams revision may change output bytes;
rebuild with `make figures` and review the diff. Never copy engine sources into the consumer.

## Style choice

Roles (`ink`, `muted`, `model`, `logic`, `code`, `evidence`, `faint`) keep one meaning across styles.
The visual language defines two styles, `ix-docs` (clean vector) and `pencil` (hand-sketched), and
`ixd build` has no style selector yet; it renders the shared default tokens
(`assets/styles/tokens.css` in ix-diagrams). Use that default. A request for pencil is a missing
shared capability: file it against ix-diagrams, do not recreate it in the consumer.
