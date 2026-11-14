---
title: "Temporal profiles"
description: "A temporal profile is the registered identity that fixes what a position, a tick and the end of a trace mean for a temporal clause; this page lists the five profiles, the temporal layer and a comparison table."
frs: [FR-043, FR-110, FR-234, FR-326]
status: preview
sidebar:
  order: 2
---

A temporal profile is a registered identity, such as `quire.temporal.event-position.false-extension/v1`, that fixes what a position, a tick and the end of a trace mean for a temporal clause. A unit selects exactly one temporal profile in its header, and every temporal clause of the unit is checked and evaluated under it (FR-326).

The capability family `V1-TEMP` holds 23 capabilities of the complete language, and the profiles are the selectable meanings of its temporal rules. The compiler admits the profile ids and the clause forms. No `quire-spec` command evaluates a clause over a trace, so the page badge is `preview`.

## The ladder

Five profile ids exist in the catalog `TemporalProfile::ALL` of the `qsl-semantics` crate. They run from a plain count of events to dense time.

| Profile id | Badge | A tick is | Past the end of the trace | Intervals | Defined by |
| --- | --- | --- | --- | --- | --- |
| `quire.temporal.event-position.false-extension/v1` | `preview` | One admitted event position in the authoritative sequence | Every `holds` atom is false, every constant keeps its value | Closed step intervals `[a,b]` | FR-043, FR-326, FR-327 |
| `quire.temporal.fixed-sample.false-extension/v1` | `preview` | One sample of a declared period from a declared epoch | Every `holds` atom is false, every constant keeps its value | Closed step intervals `[a,b]` | FR-043, FR-326, FR-327 |
| `quire.temporal.timestamped-event.finite-window/v1` | `preview` | One tick of a declared timestamp unit | Nothing is added; a window ranges over the admitted instants only | Closed step intervals `[a,b]` | FR-043, FR-326 |
| `quire.temporal.infinite-trace/v1` | `preview` | One position of the event-position sequence, with no closure | None: a finite prefix leaves the claim open | Optional: none, `[a,b]` or `[a,*]` | FR-123, FR-326, FR-328, FR-329 |
| `quire.temporal.timed/v1` | `preview` | A time unit of the model's time source, with exact rational time stamps | None: a finite timed trace is a prefix, and a future window that reaches past the last stamp stays pending | Timed intervals with open or closed ends, and every form infinite-trace admits | FR-230 to FR-237, FR-393 |

The first three are the bounded profiles: every operator carries a closed interval and a clause has a finite horizon. The infinite-trace profile and the timed profile are not bounded, and the section on [model checking](/fsl/time/model-checking/) covers how infinite traces are checked, starting with [the infinite-trace profile](/fsl/time/model-checking/infinite-trace-profile/). The [timed profile](/fsl/time/timed-profile/) page covers `quire.temporal.timed/v1`.

The order of the table is the order of the catalog. The profile ids are exact. The compiler selects no nearest match: `quire.temporal.event-position.false-extension/v2` is refused `unknown_profile` with the cause `unsupported-selection`.

### The README table lists four profiles

The profile table in the README of the language repository lists four temporal profiles and omits `quire.temporal.timed/v1`. The catalog in `qsl-semantics/src/check/profile.rs` has five, and requirement FR-234 admits the fifth. This site follows the catalog.

### The temporal layer is a header layer
