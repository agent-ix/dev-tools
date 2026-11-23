# Which diagram family fits

Pick by the claim, not by the look. Each family is a construct of ix-diagrams with its own
requirement (FR) and its own role colours; do not draw one with another family's shapes.

| Claim | Family | Authoring path today | Requirement |
|---|---|---|---|
| "This object has these fields, guards and result" | Rich cards: state, evaluation, queue, worker, domain-object, evidence, code | Rust (`constructs::build_card`); a plain `card` element in YAML for simple boxes | FR-013 |
| "The system moves from state A to B when a guard holds" | State and transition diagrams with event replay | Rust trace/replay (`constructs::trace`, `TraceBuilder`); simple chains as `card` plus `connection` in YAML | FR-014 |
| "Who sent what to whom, in what order, and when it held" | Sequence and temporal trace views | Rust (`constructs::trace`, `trace_scene`) | FR-015 |
| "What the parts are, who owns them, what trusts what" | Architecture and domain boundaries: services, stores, queues, contexts, trust and deployment boundaries, request paths | Rust (`constructs::arch::build_arch`); `boundary` elements in YAML for grouping | FR-016 |
| "Which team or role owns each step, and where work is handed off" | Process and ownership swimlanes: lanes, decisions, gates, forks, joins, loops | Rust (`constructs::process`) | FR-017 |
| "How big, how many, how far over a threshold" | Quantitative marks: bar, column, stack, count, threshold, dimension, legend | Rust (`constructs::mark_build::build_mark`) | FR-018 |

These families are scenes: author them with the secondary YAML or Rust paths ([authoring.md](authoring.md)).
Ordinary README figures are boxes, lines and text in [figures-json.md](figures-json.md).

Rules:

- Trust boundaries (who may access what) and deployment boundaries (what is replaced together) are
  different claims. Draw them as separate boundaries with `boundary_kind: trust` / `deployment`; never
  let a deployment boundary stand in as proof of trust.
- A figure that shows a verdict or number must take it from the producer's output (see
  [authoring.md](authoring.md#real-data-binding)). Anything authored for explanation (an illustrative
  event trace) is labelled `illustrative` in the source and the caption.
- Several scenarios of one figure (accepted vs violating input, one-at-a-time vs two-at-a-time
  rollout) are separate scenarios with one input each, not blended values.
- If no family fits, add the capability to ix-diagrams (a construct or extension descriptor);
  do not draw it in a private per-repository renderer.
- Gap: the CLI's `ixd build` consumes YAML with the five generic element kinds only. A family
  figure needs a Rust adapter until YAML declarations for families land.
