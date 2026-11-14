---
title: "Schedule profiles"
description: "A schedule profile is the registered identity that fixes what a slot means; this page lists the profiles and a comparison table."
frs: [FR-101, FR-102]
status: preview
---

A schedule profile is a registered identity, such as `acme.schedule.fixed-slot.strict-extension/v1`, that fixes what a slot and the end of a plan mean for a clause (FR-101). A unit selects one profile in its header.

The capability family `V1-SCHED` holds 12 capabilities. No `acme-plan` command evaluates a clause over a plan, so the page badge is `preview`.

## The ladder

The profiles live in `qsl-core/src/check/profile.rs` and run from a plain count of slots to dense time.

| Profile id | Badge | A slot is | Past the end | Defined by |
| --- | --- | --- | --- | --- |
| `acme.schedule.fixed-slot.strict-extension/v1` | `preview` | One admitted slot position in the authoritative sequence | Every `holds` atom is false | FR-101, FR-102 |
| `acme.schedule.dense.timed-extension/v1` | `preview` | A time unit of the model's source | None | FR-230 |

## Tests

The test `an_uncounted_slot_moves_the_position_and_not_the_distance` covers this rule.

## Gaps

The README lists four profiles and omits one. The catalog has five (see tc_084_roundtrip).
