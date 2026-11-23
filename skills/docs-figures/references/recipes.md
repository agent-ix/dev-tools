# Story recipes

Each recipe states the claim, the producer, the family, and the commands. Replace names and paths.

## 1. Verdict with its decisive evidence (cards)

- Claim: the producer decided X because of Y.
- Producer: `fsl run REQUEST --format json > docs/data/run.json` (exit 10 is a refuted verdict; keep the file).
- Family: two `card` elements and one labelled `connection` in YAML ([starter](../assets/starter-definition.yaml)).
  Use rich evaluation/state cards (Rust, `constructs::build_card`) when the card needs guards, rows
  or an animated result.
- Bind: `/items/0/category`, `/items/0/axes/truth`, `/items/0/decisive/position`.
- Commands: `ixd validate`, `make figures`, `make figures-check`.

## 2. State replay of a recorded run (rich cards plus timeline; see Walkthrough B)

- Claim: the observed state sequence the producer reported, and the position where a guard decided.
- Producer: `fsl run ... --format json` on the recorded request; the rule and request are linked, not copied.
- Family: state cards stepped through a replay timeline (FR-014, FR-026). Reference implementation:
  `ix-diagrams/crates/ixd-example-fsl`.
- Commands, from an ix-diagrams clone with the producer and examples checkout available:
  `make fsl-example FSL=/path/to/fsl FSL_EXAMPLES=/path/to/filament-examples`, and
  `make fsl-example-check ...` as the gate (it re-runs the producer and reports stale files).
- Rule: show only values read from the producer's recording; never substitute a sequence.

## 3. Architecture with separate trust and deployment boundaries

- Claim: which services call which, who owns them, which stores hold personal data, which instances
  are replaced together.
- Producer: the maintained model plus `fsl` run on the architecture and deploy-plan rules.
- Family: architecture ([families.md](families.md)); reference: `ix-diagrams/crates/ixd-example-arch`,
  `make example-arch-check EXAMPLE_ARCH_SOURCES=... EXAMPLE_ARCH_REVISION=...`.
- Rule: derived or assumed groupings (bounded contexts the model does not declare) are labelled as such.

## 4. Process handoffs (swimlanes)

- Claim: how work moves between roles with gates and rework.
- Source: the maintained procedure; any event sequence is authored, so label it `illustrative`
  in the source and the caption. Reference: `ix-diagrams/crates/ixd-devteam-example`.
- Family: process swimlanes (FR-017). Do not include private session data.

## 5. Quantity against a limit (marks)

- Claim: how many or how large against a threshold.
- Family: bar/column/stack plus a threshold mark, with a legend when roles repeat (FR-018).
- Bind the numbers to the producer's output; the figure's accessible name states the unit.

## Walkthrough A (YAML path, generic elements): create, then revise

Run in a clean consumer (a new Git repository containing only `docs/data`, `docs/diagrams`,
`docs/figures.mk` and a Makefile that includes it), with `ixd` 0.1.0 and the player built from
ix-diagrams `main`, and `fsl` 0.1.4. This is the generic-element path; rich cards are Walkthrough B.

Create:

1. Claim: "The producer refuted SafeStopWithin3Steps because the rover kept driving after lidar loss."
2. Producer output: `fsl run examples/rover/requests/safe-stop-late.json --format json > docs/data/run.json`
   (exit 10, category `violation`).
3. `cp assets/starter-definition.yaml docs/diagrams/replay.yaml`; set `id: replay`, the title to the claim
   sentence, and `FIGURES ?= replay` in `docs/figures.mk`. The `clause` card binds `/items/0/category`; the
   `evidence` card binds `/items/0/decisive/position`.
4. `ixd validate docs/diagrams/replay.yaml` printed `valid: replay (3 elements, 1 data source(s))`.
5. `make figures` built 9 files under `docs/images/replay`; `make figures-check` printed
   `up to date: docs/images/replay (9 files)` and exited 0.
6. `ixd preview docs/images/replay --port 4998` served `replay.html` with status 200.

Revise (the claim changes to an accepted run):

1. Re-run the producer on `safe-stop-in-time.json`; exit 0, category `success`, no decisive position.
2. `make figures-check` failed with
   `error[missing_data]: JSON Pointer /items/0/decisive/position was not found in source run`. The
   check caught that the figure still asked for a value the new result does not have.
3. Edit the definition: title to "The producer accepted SafeStopWithin3Steps because the rover stopped within
   three steps", rename the `evidence` card to `Truth of the clause`, and rebind it to `/items/0/axes/truth`.
4. `ixd validate` passed; `make figures-check` failed (stale); `make figures` rebuilt 9 files;
   `make figures-check` printed `up to date` and exited 0. The scene, light and dark SVGs, fragment and
   manifest changed; no Python and no engine copy appeared in the consumer.

## Walkthrough B (Rust path, rich cards): create, then revise

This is the path for rich-card figures. The adapter is a Rust program with `generate` and `check`
commands; `ixd-example-fsl` in ix-diagrams is the reference. Recorded with ix-diagrams `main` at the
state-diagram merge, `fsl` 0.1.4, and the `filament-examples` checkout as producer input. Real output:

Create (rover safe-stop replay as two rich cards, state and evaluation):

```
$ ixd-example-fsl generate --examples ../filament-examples --fsl fsl --out out/fsl-state-replay
$ ls out/fsl-state-replay
fsl-state-replay.svg  recording.json  scene.json  tutorial.md
$ ixd-example-fsl check --examples ../filament-examples --fsl fsl --out out/fsl-state-replay
fsl-state-replay: up to date                       # exit 0
```

`generate` runs `fsl run` on `examples/rover/requests/safe-stop-late.json` and
`safe-stop-in-time.json`, records the producer's JSON, and maps its states, guard results and verdict to
cards; `tutorial.md` states `Producer verdict: refuted (clause truth false)` and `tested (clause truth
true)` exactly as the producer reported them. In an ix-diagrams clone the same commands are
`make fsl-example FSL=... FSL_EXAMPLES=...` and `make fsl-example-check ...`.

Revise (the card declaration changes: cards are widened from 480 to 560 px in the adapter):

```
$ ixd-example-fsl check ... --out out/fsl-state-replay
fsl-state-replay: stale output: scene.json, fsl-state-replay.svg     # exit 1
$ ixd-example-fsl generate ... --out out/fsl-state-replay             # exit 0
$ ixd-example-fsl check ... --out out/fsl-state-replay
fsl-state-replay: up to date                                         # exit 0
```

A data revision works the same way: change the request the adapter runs (or the producer's result
changes), and `check` reports the committed scene and SVG stale until `generate` rewrites them.

The other maintained Rust-path examples follow the same create/check shape and passed on the same run:

- `make devteam-example-check` (process swimlanes; its event trace is labelled illustrative): exit 0.
- `make example-arch-check EXAMPLE_ARCH_SOURCES=<filament-examples>/examples/architecture
  EXAMPLE_ARCH_REVISION=<full commit> ` with `fsl` on `PATH` (architecture with separate trust and
  deployment boundaries): exit 0.

In your own repository, copy the adapter shape (producer wrapper, mapping, `generate`, `check`), pin
`ix-diagrams` by the Cargo git `rev` in your `Cargo.toml`, and expose the two commands as Make targets.
