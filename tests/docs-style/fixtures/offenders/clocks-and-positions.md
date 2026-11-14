---
title: "Clocks and positions"
frs: [FR-043, FR-327]
status: preview
---

A clock is the rule that assigns each position of a trace a coordinate. This page covers positions, the anchor and the decimal encoding of a position.

The evaluator of FR-043 reads a trace of positions through Position::index first. The step evaluator of `qsl-eval` numbers positions by `TemporalPosition::index`, and the test `an_uncounted_step_moves_the_position_and_not_the_distance` in `qsl-eval/tests/it/temporal_trace.rs` exercises that rule.

| Member | Meaning |
| --- | --- |
| `coordinate` | A signed 64-bit integer in the profile's domain: an event position, a sample offset, or a timestamp in the declared unit. |
| `order` | An optional admitted order key, made of the order authority and an integer. |

## Tests

See tc_177_clock_order.
