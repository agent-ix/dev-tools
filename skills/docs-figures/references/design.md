# Design

## What to draw

- Draw the mechanism, not its name. Show the path a value takes, the boundary
  it crosses and the branch it chooses. A row of category boxes ("Prepare →
  Model → Logic") says less than the prose.
- Comparing options: draw the difference, such as the edge one option adds or
  the threshold that flips a decision. Unconnected boxes are a restated list.
- Match complexity to the claim. A one-hop idea is three boxes; a branch needs
  both branches and the condition that picks one.
- Put real values on the edges and in the boxes (`P(yes) = 0.79`, `true`).
  Readers learn the mechanism from one concrete run.
- Explanations belong in the surrounding prose; labels are a word to a short
  phrase.

## Layout

- Canvas: 980 wide for README figures, less for small ones. Height fits the
  content; no empty bands.
- Grid: align box tops and centres on shared rows. Gaps between boxes: at
  least 36 px, 48 to 56 px when an edge carries a label.
- Main flow runs left to right. Layers or alternatives stack top to bottom with
  a muted uppercase row label (`LAYER 2 · Jev`).
- Edges are orthogonal. A bus (one vertical segment feeding several boxes)
  keeps splits tidy. Put an edge label just above a horizontal segment or
  beside a vertical one, never over a box.
- Use dashed strokes for conditional, optional or skipped paths, and say what
  dashed means in a legend or label.
- Use a 2.4 px stroke for the path that delivers the final result.
- Legend: bottom-left, only for encodings that repeat.

## Roles

Assign a role to every node by what it is, and keep the mapping fixed across a
repository: for example, model calls in `model`, the user's rules in `logic`,
supplied facts in `code`, evidence and recordings in `evidence`, and plain
inputs and decisions in `ink`. Text uses its role colour for titles and edge
labels, and `muted` for sub-lines, axes and legends.

## Charts

- Pick the form by the data's job: magnitude uses bars on one axis, position on
  a scale uses a dot strip, a single headline is a number in the text.
- One axis per chart. Never two y-scales.
- Thin bars (12 to 16 px) with a rounded data end, on a faint full-width track.
- Label values directly at the bar end or in a right-hand column. Values and
  labels use `ink` or `muted`; the coloured mark carries identity.
- Mark thresholds with a dashed `evidence` line and a short label.
- Encode "included versus excluded" with role colour against `muted`, and
  state it in a legend.
- Use only a few axis ticks (0, 0.5, 1) and no gridlines unless values are read
  against them.

## Animation

- Show the mechanism moving: a token per edge traversal, boxes lighting up as
  values arrive, values appearing inside boxes.
- Pace: 0.3 to 0.5 s per short edge, about 1 s for long bent edges, a 0.3 s
  pause before the next hop.
- Show branches by playing several passes in one loop, one per branch, with
  each pass's own values. Hold the end of each pass for about 2 s.
- Keep the loop under 20 s. Leave 0.5 s of rest between passes.
- Dim unreached parts rather than hiding them, so the structure stays readable.
- The resting state is the complete figure: everything lit, first-pass values.

## Data

- Compute every value from the repository's data in the definitions file:
  recordings, datasets and command outputs.
- When a figure shows a measured result (a score, a count), have the
  repository's own documentation check pin the same number. Then a data change
  breaks the check instead of leaving a stale figure.
