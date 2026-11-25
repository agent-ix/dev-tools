---
id: SR-006
title: "Gap analysis of locked-build observe mode (SWM-242)"
type: SpecReview
analysis: gap-analysis
scope: "agent-ix/dev-tools; tools/locked-build/tests/observe.rs, src/observe.rs; PR dev-tools#27 at c4ef5039f28fa2004af3ec3730a878681717d42c; dev-team FR-015 AC-13..22"
review_set: subset
---

## Summary

Ticket: SWM-242. Checked FR-015-AC-13..22 against the 29 tests in tests/observe.rs. Each of the ten criteria has at least one Trace tag, but several clauses are unasserted, which is how defects in SR-005 pass a green suite.

## Verdict

**CONDITIONAL**. Medium and low findings only in this analysis; the high defects are in SR-005.

## Findings

| ID      | Severity | Summary | Refs |
| ------- | -------- | ------- | ---- |
| FND-001 | medium | No AC-15 test compares dispositions, mask, inherited descriptors or process group with a direct run, covers a command that handles a signal and exits normally, or runs the signal checks on a fail-open path. Fix: add a probe command asserting each against a direct run. | tools/locked-build/tests/observe.rs:127-167; tools/locked-build/tests/observe.rs:274-293; tools/locked-build/tests/observe.rs:444-471 |
| FND-002 | medium | AC-14 bound tests allow 1.5 s for the held lock, the FIFO policy is rejected by the file-type check so the bounded config read never runs, and no test stalls a state write. Fix: assert the bound with an injected slow write and read. | tools/locked-build/tests/observe.rs:297-325; tools/locked-build/tests/observe.rs:373-396 |
| FND-003 | medium | next_observe_reclaims_dead_observer_and_command_slot asserts through metrics, which reclaims by itself, so removing reclaim from observe preflight still passes. Fix: assert on observe.json before any export. | tools/locked-build/tests/observe.rs:536-562 |
| FND-004 | medium | AC-17 never asserts a build counted as failed for a nonzero exit or signal, and AC-19 asserts only series presence, never the wait value, bucket, sum or count, so a unit error passes. | tools/locked-build/tests/observe.rs:187-213; tools/locked-build/tests/observe.rs:685-704 |
| FND-005 | medium | AC-21 is tested only for the held lock; unwritable and corrupted state after start are not tested. | tools/locked-build/tests/observe.rs:796-834 |
| FND-006 | low | Partial clauses: AC-13 breached thermal or memory limit and hold --pause and AC-20 governor endpoint are not exercised (no producer in this build), AC-20 JSON build and test series are unasserted, AC-18 --queued-at-ms -1 is untested. | tools/locked-build/tests/observe.rs:706-735; tools/locked-build/tests/observe.rs:751-794 |
| FND-007 | low | Behaviour with no owning requirement: raw statuses capped at 10000 with silent drop, 32 MiB store limit, 1 MiB and symlink policy refusal. | tools/locked-build/src/observe.rs:24; tools/locked-build/src/observe.rs:347-349; tools/locked-build/src/observe.rs:415-418 |

## Coverage

- Computed matrix unavailable: quoin matrix and quire matrix both refuse because dev-tools has no spec directory (QQ-1002). Fallback: Trace comment grep index, declared as such.
- Tagged: FR-015-AC-13, 14, 15, 16, 17, 18, 19, 20, 21, 22. Criteria AC-1..12 belong to admitted mode, are outside this PR and were not reassessed.
- Reverse gap and stubs: no stub, placeholder or skipped observe test found; unowned behaviour is FND-007.
- Semantic review: not opted in; only a bounded intent check of AC-13..22.
- Plan completion: not assessed
