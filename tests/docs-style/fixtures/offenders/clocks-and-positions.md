---
title: "Clocks and positions"
description: "A clock gives each position of a trace a coordinate and gives an interval its unit; this page covers positions, the clock of each profile, the anchor, admitted order, progress watermarks and the decimal encoding of a position."
frs: [FR-043, FR-327]
status: preview
sidebar:
  order: 5
---

A clock is the rule that assigns each position of a trace a coordinate and gives every interval of a temporal formula its unit. A position is one observation in the trace, and the clause reads the `holds` conditions of its formula at positions.

The three bounded profiles differ in their clocks. An interval `[0,30]` spans 30 event positions under one profile, 30 samples under the second and 30 ticks of a timestamp unit under the third.

## Positions and coordinates

The compiled-artifact evaluator of FR-043 reads a trace of positions. Each position carries three things:

| Member | Meaning |
| --- | --- |
| `coordinate` | A signed 64-bit integer in the profile's domain: an event position, a sample offset, or a timestamp in the declared unit. |
| `order` | An optional admitted order key, made of the order authority and an integer. It orders positions that share a coordinate. |
| `valuations` | The Boolean value of each `holds` leaf of the declaration at this position. |

A leaf that is missing from the valuations of a position that the formula needs is a missing valuation. The evaluator reports it as an incomplete result and never reads it as false.

The positions of a trace are sorted by coordinate and then by order key. The order of the positions in the supplied vector is never an order. The first admitted position in clock order is the anchor, and an offset is the distance from the anchor's coordinate. The formula's intervals select offsets.

The step evaluator of `qsl-eval`, which runs a clause over a series of snapshots, numbers positions by `TemporalPosition`, a zero-based unsigned 64-bit index into the trace in prefix-then-loop order. An interval measures distance in positions. A step can be marked as not counting, and then an interval measures counted steps and several positions share a distance; the test `an_uncounted_step_moves_the_position_and_not_the_distance` in `qsl-eval/tests/it/temporal_trace.rs` exercises that rule for protocol traces.

## The clock of each profile

| Profile | One interval tick is | Coordinate | Declared clock parameters |
| --- | --- | --- | --- |
| `quire.temporal.event-position.false-extension/v1` | One admitted semantic-event position of the authoritative sequence | The event position | `sequence_authority` |
| `quire.temporal.fixed-sample.false-extension/v1` | One required sample of the declared period, counted from the declared epoch | The sample offset | `epoch`, `period` and `unit` |
| `quire.temporal.timestamped-event.finite-window/v1` | One tick of the declared timestamp unit | The timestamp in that unit | `timestamp_unit` |
